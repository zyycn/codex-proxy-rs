//! 数据面执行观测的非阻塞 PostgreSQL 写入队列。

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use gateway_core::engine::{
    AttemptRecord, CancellationToken, ExecutionStore, IntermediateFailure,
    ModelRequestFinalization, ModelRequestId, NewModelRequest, ProbeFailure, RecoveryReport,
    UpstreamSendState,
};
use gateway_core::error::StoreError;
use gateway_core::task::{DaemonTask, WorkerTaskError};
use tokio::sync::{Mutex, mpsc};

const DEFAULT_QUEUE_CAPACITY: usize = 4_096;

/// 将数据面观测写入转换为有界、非阻塞的进程内命令。
///
/// 队列满、worker 尚未启动或已经退出时只丢弃观测并记录告警；协议数据面不会
/// 等待 PostgreSQL，也不会看到 Store 错误。启动恢复仍直接访问底层 Store。
pub struct BufferedExecutionStore<S: ?Sized> {
    inner: Arc<S>,
    sender: mpsc::Sender<ExecutionObservationWrite>,
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
        let (sender, receiver) = mpsc::channel(capacity.get());
        (
            Self {
                inner: Arc::clone(&inner),
                sender,
            },
            ExecutionObservationWriter {
                inner,
                receiver: Mutex::new(receiver),
            },
        )
    }

    fn enqueue(&self, write: ExecutionObservationWrite) {
        if let Err(error) = self.sender.try_send(write) {
            let (reason, write) = match error {
                mpsc::error::TrySendError::Full(write) => ("full", write),
                mpsc::error::TrySendError::Closed(write) => ("closed", write),
            };
            tracing::warn!(
                operation = write.operation(),
                reason,
                "执行观测队列不可用，已丢弃本次写入"
            );
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
    receiver: Mutex<mpsc::Receiver<ExecutionObservationWrite>>,
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
                let write = tokio::select! {
                    () = cancellation.cancelled() => return Ok(()),
                    write = receiver.recv() => write,
                };
                let Some(write) = write else {
                    return Err(WorkerTaskError::safe("execution observation queue closed"));
                };
                let operation = write.operation();
                if let Err(error) = write.persist(self.inner.as_ref()).await {
                    tracing::warn!(
                        operation,
                        error_kind = ?error.kind(),
                        "执行观测写入失败，数据面不受影响"
                    );
                }
            }
        })
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
