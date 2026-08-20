//! 数据面执行观测的非阻塞 PostgreSQL 写入队列。

use std::mem::size_of;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use gateway_core::engine::{
    AttemptRecord, CancellationToken, ExecutionStore, IntermediateFailure,
    ModelRequestFinalization, ModelRequestId, NewModelRequest, ProbeFailure, RecoveryReport,
    UpstreamSendState,
};
use gateway_core::error::{ProviderError, StoreError};
use gateway_core::task::{DaemonTask, WorkerTaskError};
use tokio::sync::{Mutex, mpsc};

const DEFAULT_QUEUE_CAPACITY: usize = 4_096;
const DEFAULT_QUEUE_BYTE_CAPACITY: usize = 64 * 1024 * 1024;
const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// 执行观测缓冲区的进程内累计状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionBufferStats {
    /// 尚未完成落库的队列项和当前写入项数量。
    pub queued_items: usize,
    /// 尚未完成落库的观测对象估算字节数。
    pub queued_bytes: usize,
    /// 进程启动后成功入队的累计数量。
    pub enqueued_total: u64,
    /// 因队列、字节预算或关闭排空超时丢弃的累计数量。
    pub dropped_total: u64,
    /// 已成功写入底层 Store 的累计数量。
    pub persisted_total: u64,
    /// 底层 Store 返回失败的累计数量。
    pub write_failure_total: u64,
}

struct ExecutionBufferState {
    maximum_queued_bytes: usize,
    queued_items: AtomicUsize,
    queued_bytes: AtomicUsize,
    enqueued_total: AtomicU64,
    dropped_total: AtomicU64,
    persisted_total: AtomicU64,
    write_failure_total: AtomicU64,
}

impl ExecutionBufferState {
    fn new(maximum_queued_bytes: NonZeroUsize) -> Self {
        Self {
            maximum_queued_bytes: maximum_queued_bytes.get(),
            queued_items: AtomicUsize::new(0),
            queued_bytes: AtomicUsize::new(0),
            enqueued_total: AtomicU64::new(0),
            dropped_total: AtomicU64::new(0),
            persisted_total: AtomicU64::new(0),
            write_failure_total: AtomicU64::new(0),
        }
    }

    fn reserve(&self, bytes: usize) -> bool {
        let reserved = self
            .queued_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(bytes)
                    .filter(|next| *next <= self.maximum_queued_bytes)
            })
            .is_ok();
        if reserved {
            self.queued_items.fetch_add(1, Ordering::Relaxed);
        }
        reserved
    }

    fn release(&self, bytes: usize) {
        self.queued_bytes.fetch_sub(bytes, Ordering::AcqRel);
        self.queued_items.fetch_sub(1, Ordering::AcqRel);
    }

    fn record_enqueued(&self) {
        saturating_increment(&self.enqueued_total, 1);
    }

    fn record_dropped(&self, count: usize) {
        saturating_increment(
            &self.dropped_total,
            u64::try_from(count).unwrap_or(u64::MAX),
        );
    }

    fn record_persisted(&self) {
        saturating_increment(&self.persisted_total, 1);
    }

    fn record_write_failure(&self) {
        saturating_increment(&self.write_failure_total, 1);
    }

    fn snapshot(&self) -> ExecutionBufferStats {
        ExecutionBufferStats {
            queued_items: self.queued_items.load(Ordering::Acquire),
            queued_bytes: self.queued_bytes.load(Ordering::Acquire),
            enqueued_total: self.enqueued_total.load(Ordering::Acquire),
            dropped_total: self.dropped_total.load(Ordering::Acquire),
            persisted_total: self.persisted_total.load(Ordering::Acquire),
            write_failure_total: self.write_failure_total.load(Ordering::Acquire),
        }
    }
}

fn saturating_increment(counter: &AtomicU64, increment: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(increment))
    });
}

/// 将数据面观测写入转换为有界、非阻塞的进程内命令。
///
/// 队列满、worker 尚未启动或已经退出时只丢弃观测并记录告警；协议数据面不会
/// 等待 PostgreSQL，也不会看到 Store 错误。启动恢复仍直接访问底层 Store。
pub struct BufferedExecutionStore<S: ?Sized> {
    inner: Arc<S>,
    sender: mpsc::Sender<QueuedExecutionObservation>,
    state: Arc<ExecutionBufferState>,
}

impl<S: ?Sized> BufferedExecutionStore<S> {
    #[must_use]
    pub fn new(inner: Arc<S>) -> (Self, ExecutionObservationWriter<S>) {
        Self::with_capacity(
            inner,
            NonZeroUsize::new(DEFAULT_QUEUE_CAPACITY).expect("queue capacity is non-zero"),
        )
    }

    #[must_use]
    pub fn with_capacity(
        inner: Arc<S>,
        capacity: NonZeroUsize,
    ) -> (Self, ExecutionObservationWriter<S>) {
        Self::with_limits(
            inner,
            capacity,
            NonZeroUsize::new(DEFAULT_QUEUE_BYTE_CAPACITY)
                .expect("queue byte capacity is non-zero"),
        )
    }

    #[must_use]
    pub fn with_limits(
        inner: Arc<S>,
        capacity: NonZeroUsize,
        maximum_queued_bytes: NonZeroUsize,
    ) -> (Self, ExecutionObservationWriter<S>) {
        let (sender, receiver) = mpsc::channel(capacity.get());
        let state = Arc::new(ExecutionBufferState::new(maximum_queued_bytes));
        (
            Self {
                inner: Arc::clone(&inner),
                sender,
                state: Arc::clone(&state),
            },
            ExecutionObservationWriter {
                inner,
                receiver: Mutex::new(receiver),
                state,
            },
        )
    }

    #[must_use]
    pub fn stats(&self) -> ExecutionBufferStats {
        self.state.snapshot()
    }

    fn enqueue(&self, write: ExecutionObservationWrite) {
        let estimated_bytes = write
            .estimated_bytes()
            .saturating_add(write.request_id().map_or(0, str::len))
            .max(1);
        if !self.state.reserve(estimated_bytes) {
            self.state.record_dropped(1);
            let stats = self.state.snapshot();
            tracing::warn!(
                operation = write.operation(),
                request_id = ?write.request_id(),
                reason = "byte_capacity",
                estimated_bytes,
                maximum_queued_bytes = self.state.maximum_queued_bytes,
                dropped_total = stats.dropped_total,
                "执行观测队列字节预算不足，已丢弃本次写入"
            );
            return;
        }
        let queued =
            QueuedExecutionObservation::new(write, estimated_bytes, Arc::clone(&self.state));
        match self.sender.try_send(queued) {
            Ok(()) => self.state.record_enqueued(),
            Err(error) => {
                let (reason, queued) = match error {
                    mpsc::error::TrySendError::Full(queued) => ("full", queued),
                    mpsc::error::TrySendError::Closed(queued) => ("closed", queued),
                };
                let operation = queued.operation();
                let request_id = queued.request_id().map(ToOwned::to_owned);
                drop(queued);
                self.state.record_dropped(1);
                let stats = self.state.snapshot();
                tracing::warn!(
                    operation,
                    request_id = ?request_id,
                    reason,
                    queued_items = stats.queued_items,
                    queued_bytes = stats.queued_bytes,
                    dropped_total = stats.dropped_total,
                    "执行观测队列不可用，已丢弃本次写入"
                );
            }
        }
    }
}

#[async_trait]
impl<S> ExecutionStore for BufferedExecutionStore<S>
where
    S: ExecutionStore + ?Sized,
{
    async fn create_model_request(&self, request: NewModelRequest) -> Result<(), StoreError> {
        self.enqueue(ExecutionObservationWrite::Create(Box::new(request)));
        Ok(())
    }

    async fn record_attempt(&self, attempt: AttemptRecord) -> Result<(), StoreError> {
        self.enqueue(ExecutionObservationWrite::Attempt(Box::new(attempt)));
        Ok(())
    }

    async fn create_model_request_with_attempt(
        &self,
        request: NewModelRequest,
        attempt: AttemptRecord,
    ) -> Result<(), StoreError> {
        self.enqueue(ExecutionObservationWrite::CreateWithAttempt(Box::new((
            request, attempt,
        ))));
        Ok(())
    }

    async fn mark_send_state(
        &self,
        request_id: &ModelRequestId,
        state: UpstreamSendState,
    ) -> Result<(), StoreError> {
        self.enqueue(ExecutionObservationWrite::MarkSendState {
            request_id: request_id.clone(),
            state,
        });
        Ok(())
    }

    async fn mark_downstream_committed(
        &self,
        request_id: &ModelRequestId,
        committed_at: SystemTime,
        client_status_code: Option<u16>,
    ) -> Result<(), StoreError> {
        self.enqueue(ExecutionObservationWrite::MarkDownstreamCommitted {
            request_id: request_id.clone(),
            committed_at,
            client_status_code,
        });
        Ok(())
    }

    async fn record_client_status(
        &self,
        request_id: &ModelRequestId,
        client_status_code: u16,
    ) -> Result<(), StoreError> {
        self.enqueue(ExecutionObservationWrite::RecordClientStatus {
            request_id: request_id.clone(),
            client_status_code,
        });
        Ok(())
    }

    async fn record_intermediate_failure(
        &self,
        failure: IntermediateFailure,
    ) -> Result<(), StoreError> {
        self.enqueue(ExecutionObservationWrite::IntermediateFailure(Box::new(
            failure,
        )));
        Ok(())
    }

    async fn record_probe_failure(&self, failure: ProbeFailure) -> Result<(), StoreError> {
        self.enqueue(ExecutionObservationWrite::ProbeFailure(Box::new(failure)));
        Ok(())
    }

    async fn finalize_model_request(
        &self,
        finalization: ModelRequestFinalization,
    ) -> Result<(), StoreError> {
        self.enqueue(ExecutionObservationWrite::Finalize(Box::new(finalization)));
        Ok(())
    }

    async fn recover_expired(&self, now: SystemTime) -> Result<RecoveryReport, StoreError> {
        self.inner.recover_expired(now).await
    }
}

/// 由 Host 监督的单消费者，保证同一请求的观测命令按入队顺序落库。
pub struct ExecutionObservationWriter<S: ?Sized> {
    inner: Arc<S>,
    receiver: Mutex<mpsc::Receiver<QueuedExecutionObservation>>,
    state: Arc<ExecutionBufferState>,
}

impl<S: ?Sized> ExecutionObservationWriter<S> {
    #[must_use]
    pub fn stats(&self) -> ExecutionBufferStats {
        self.state.snapshot()
    }
}

impl<S> DaemonTask for ExecutionObservationWriter<S>
where
    S: ExecutionStore + ?Sized,
{
    fn run(
        &self,
        cancellation: CancellationToken,
    ) -> futures::future::BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let mut receiver = self.receiver.lock().await;
            loop {
                let queued = tokio::select! {
                    () = cancellation.cancelled() => {
                        drain_on_shutdown(
                            &mut receiver,
                            self.inner.as_ref(),
                            self.state.as_ref(),
                        ).await;
                        return Ok(());
                    },
                    queued = receiver.recv() => queued,
                };
                let Some(queued) = queued else {
                    return Err(WorkerTaskError::safe("execution observation queue closed"));
                };
                persist_queued(queued, self.inner.as_ref()).await;
            }
        })
    }
}

async fn persist_queued<S>(mut queued: QueuedExecutionObservation, store: &S)
where
    S: ExecutionStore + ?Sized,
{
    let operation = queued.operation();
    let request_id = queued.request_id().map(ToOwned::to_owned);
    let state = Arc::clone(&queued.state);
    let Some(write) = queued.take_write() else {
        state.record_write_failure();
        tracing::error!(
            operation,
            request_id = ?request_id,
            "执行观测队列内部状态无效，已丢弃本次写入"
        );
        return;
    };
    // Store 写入没有统一幂等键；超时可能表示已提交，不能在这里盲目重试并
    // 制造重复 ops_events。失败会被计数并丢弃，数据面始终不等待补偿。
    match write.persist(store).await {
        Ok(()) => state.record_persisted(),
        Err(error) => {
            state.record_write_failure();
            let stats = state.snapshot();
            tracing::warn!(
                operation,
                request_id = ?request_id,
                error_kind = ?error.kind(),
                write_failure_total = stats.write_failure_total,
                "执行观测写入失败，数据面不受影响"
            );
        }
    }
}

async fn drain_on_shutdown<S>(
    receiver: &mut mpsc::Receiver<QueuedExecutionObservation>,
    store: &S,
    state: &ExecutionBufferState,
) where
    S: ExecutionStore + ?Sized,
{
    receiver.close();
    let queued_at_shutdown = receiver.len();
    let started_at = Instant::now();
    let mut dropped = 0_usize;

    while let Some(queued) = receiver.recv().await {
        let remaining = SHUTDOWN_DRAIN_TIMEOUT.saturating_sub(started_at.elapsed());
        if remaining.is_zero() {
            drop(queued);
            dropped = dropped.saturating_add(1);
            break;
        }
        if tokio::time::timeout(remaining, persist_queued(queued, store))
            .await
            .is_err()
        {
            dropped = dropped.saturating_add(1);
            break;
        }
    }
    while let Ok(queued) = receiver.try_recv() {
        drop(queued);
        dropped = dropped.saturating_add(1);
    }

    if dropped > 0 {
        state.record_dropped(dropped);
        let stats = state.snapshot();
        tracing::warn!(
            queued_at_shutdown,
            dropped,
            dropped_total = stats.dropped_total,
            drain_timeout_ms =
                u64::try_from(SHUTDOWN_DRAIN_TIMEOUT.as_millis()).unwrap_or(u64::MAX),
            "执行观测队列关闭排空超时，剩余写入已丢弃"
        );
    } else if queued_at_shutdown > 0 {
        tracing::info!(queued_at_shutdown, "执行观测队列已在关闭前排空");
    }
}

struct QueuedExecutionObservation {
    write: Option<ExecutionObservationWrite>,
    operation: &'static str,
    request_id: Option<String>,
    estimated_bytes: usize,
    state: Arc<ExecutionBufferState>,
}

impl QueuedExecutionObservation {
    fn new(
        write: ExecutionObservationWrite,
        estimated_bytes: usize,
        state: Arc<ExecutionBufferState>,
    ) -> Self {
        let operation = write.operation();
        let request_id = write.request_id().map(ToOwned::to_owned);
        Self {
            write: Some(write),
            operation,
            request_id,
            estimated_bytes,
            state,
        }
    }

    fn operation(&self) -> &'static str {
        self.operation
    }

    fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    fn take_write(&mut self) -> Option<ExecutionObservationWrite> {
        self.write.take()
    }
}

impl Drop for QueuedExecutionObservation {
    fn drop(&mut self) {
        self.state.release(self.estimated_bytes);
    }
}

enum ExecutionObservationWrite {
    Create(Box<NewModelRequest>),
    Attempt(Box<AttemptRecord>),
    CreateWithAttempt(Box<(NewModelRequest, AttemptRecord)>),
    MarkSendState {
        request_id: ModelRequestId,
        state: UpstreamSendState,
    },
    MarkDownstreamCommitted {
        request_id: ModelRequestId,
        committed_at: SystemTime,
        client_status_code: Option<u16>,
    },
    RecordClientStatus {
        request_id: ModelRequestId,
        client_status_code: u16,
    },
    IntermediateFailure(Box<IntermediateFailure>),
    ProbeFailure(Box<ProbeFailure>),
    Finalize(Box<ModelRequestFinalization>),
}

impl ExecutionObservationWrite {
    const fn operation(&self) -> &'static str {
        match self {
            Self::Create(_) => "create_model_request",
            Self::Attempt(_) => "record_attempt",
            Self::CreateWithAttempt(_) => "create_model_request_with_attempt",
            Self::MarkSendState { .. } => "mark_send_state",
            Self::MarkDownstreamCommitted { .. } => "mark_downstream_committed",
            Self::RecordClientStatus { .. } => "record_client_status",
            Self::IntermediateFailure(_) => "record_intermediate_failure",
            Self::ProbeFailure(_) => "record_probe_failure",
            Self::Finalize(_) => "finalize_model_request",
        }
    }

    fn request_id(&self) -> Option<&str> {
        match self {
            Self::Create(request) => Some(request.id.as_str()),
            Self::Attempt(attempt) => Some(attempt.request_id.as_str()),
            Self::CreateWithAttempt(write) => Some(write.0.id.as_str()),
            Self::MarkSendState { request_id, .. }
            | Self::MarkDownstreamCommitted { request_id, .. }
            | Self::RecordClientStatus { request_id, .. } => Some(request_id.as_str()),
            Self::IntermediateFailure(failure) => Some(failure.request_id.as_str()),
            Self::ProbeFailure(_) => None,
            Self::Finalize(finalization) => Some(finalization.request_id.as_str()),
        }
    }

    fn estimated_bytes(&self) -> usize {
        size_of::<Self>().saturating_add(match self {
            Self::Create(request) => new_request_bytes(request),
            Self::Attempt(attempt) => attempt_bytes(attempt),
            Self::CreateWithAttempt(write) => {
                new_request_bytes(&write.0).saturating_add(attempt_bytes(&write.1))
            }
            Self::MarkSendState { request_id, .. }
            | Self::MarkDownstreamCommitted { request_id, .. }
            | Self::RecordClientStatus { request_id, .. } => request_id.as_str().len(),
            Self::IntermediateFailure(failure) => text_bytes([
                Some(failure.request_id.as_str()),
                Some(failure.provider_kind.as_str()),
                failure.account_id.as_ref().map(|value| value.as_str()),
                failure
                    .upstream_model_id
                    .as_ref()
                    .map(|model| model.as_str()),
                failure.upstream_request_id.as_deref(),
            ])
            .saturating_add(provider_error_bytes(&failure.error)),
            Self::ProbeFailure(failure) => text_bytes([
                Some(failure.provider_kind.as_str()),
                Some(failure.account_id.as_str()),
                Some(failure.upstream_model_id.as_str()),
            ])
            .saturating_add(provider_error_bytes(&failure.error)),
            Self::Finalize(finalization) => {
                let error_bytes = finalization.error.as_ref().map_or(0, |error| {
                    text_bytes([
                        Some(error.client_message()),
                        error.client_error_code(),
                        error.client_error_type(),
                    ])
                });
                text_bytes([
                    Some(finalization.request_id.as_str()),
                    finalization.client_response_id.as_deref(),
                    finalization.upstream_request_id.as_deref(),
                    finalization.upstream_response_id.as_deref(),
                    finalization.upstream_transport.as_deref(),
                    finalization.http_version.as_deref(),
                    finalization.websocket_pool.as_deref(),
                    finalization.service_tier.as_deref(),
                    finalization.provider_metadata_json.as_deref(),
                    finalization.provider_error_code.as_deref(),
                ])
                .saturating_add(error_bytes)
                .saturating_add(size_of::<ModelRequestFinalization>())
            }
        })
    }

    async fn persist<S>(self, store: &S) -> Result<(), StoreError>
    where
        S: ExecutionStore + ?Sized,
    {
        match self {
            Self::Create(request) => store.create_model_request(*request).await,
            Self::Attempt(attempt) => store.record_attempt(*attempt).await,
            Self::CreateWithAttempt(write) => {
                let (request, attempt) = *write;
                store
                    .create_model_request_with_attempt(request, attempt)
                    .await
            }
            Self::MarkSendState { request_id, state } => {
                store.mark_send_state(&request_id, state).await
            }
            Self::MarkDownstreamCommitted {
                request_id,
                committed_at,
                client_status_code,
            } => {
                store
                    .mark_downstream_committed(&request_id, committed_at, client_status_code)
                    .await
            }
            Self::RecordClientStatus {
                request_id,
                client_status_code,
            } => {
                store
                    .record_client_status(&request_id, client_status_code)
                    .await
            }
            Self::IntermediateFailure(failure) => store.record_intermediate_failure(*failure).await,
            Self::ProbeFailure(failure) => store.record_probe_failure(*failure).await,
            Self::Finalize(finalization) => store.finalize_model_request(*finalization).await,
        }
    }
}

fn new_request_bytes(request: &NewModelRequest) -> usize {
    size_of::<NewModelRequest>().saturating_add(text_bytes([
        Some(request.id.as_str()),
        request
            .client_api_key_id
            .as_ref()
            .map(|value| value.as_str()),
        Some(request.client_api_key_ref.as_str()),
        Some(request.protocol.as_str()),
        Some(request.endpoint.as_str()),
        Some(request.client_transport.as_str()),
        request.requested_model.as_ref().map(|model| model.as_str()),
        request.user_agent.as_deref(),
        request.reasoning_effort.as_deref(),
        request.reasoning_preset.as_deref(),
        request.request_kind.as_deref(),
        request.subagent_kind.as_deref(),
    ]))
}

fn attempt_bytes(attempt: &AttemptRecord) -> usize {
    size_of::<AttemptRecord>().saturating_add(text_bytes([
        Some(attempt.request_id.as_str()),
        Some(attempt.provider_kind.as_str()),
        attempt
            .provider_account_id
            .as_ref()
            .map(|value| value.as_str()),
        attempt
            .provider_account_ref
            .as_ref()
            .map(|value| value.as_str()),
        attempt
            .upstream_model_id
            .as_ref()
            .map(|model| model.as_str()),
        Some(attempt.upstream_transport.as_str()),
        attempt.http_version.as_deref(),
    ]))
}

fn provider_error_bytes(error: &ProviderError) -> usize {
    let mut bytes = text_bytes([
        error.upstream_code().map(|value| value.as_str()),
        error.upstream_request_id().map(|value| value.as_str()),
        error.upstream_response_id().map(|value| value.as_str()),
    ]);
    if let Some(client_error) = error.client_visible_upstream_error() {
        bytes = bytes.saturating_add(text_bytes([
            Some(client_error.message()),
            client_error.code(),
            client_error.error_type(),
        ]));
    }
    if let Some(response) = error.client_visible_upstream_response() {
        bytes = bytes
            .saturating_add(response.body().len())
            .saturating_add(response.content_type().map_or(0, <[u8]>::len));
        for header in response.headers() {
            bytes = bytes
                .saturating_add(header.name().len())
                .saturating_add(header.value().len());
        }
    }
    bytes
}

fn text_bytes<const N: usize>(values: [Option<&str>; N]) -> usize {
    values
        .into_iter()
        .flatten()
        .fold(0, |total, value| total.saturating_add(value.len()))
}
