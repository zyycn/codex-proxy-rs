//! 可由 TTL 或后续反馈收敛的 Redis 写副作用队列。
//!
//! Continuation affinity 决定下一轮 Provider 与账号，必须在响应 ID 可复用前
//! 获得 Redis 确认，因此不属于本模块的可丢失副作用。

use std::num::NonZeroUsize;
use std::sync::Arc;

use futures::future::{BoxFuture, ready};
use gateway_core::engine::admission::{
    ClientAdmissionDecision, ClientAdmissionError, ClientAdmissionPort, ClientAdmissionRecovery,
    ClientAdmissionRequest, ClientAdmissionRestoreResult,
};
use gateway_core::engine::execution::{
    ProviderCircuitDecision, ProviderCircuitError, ProviderCircuitPort,
};
use gateway_core::engine::{CancellationToken, ModelRequestId};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::routing::ProviderKind;
use gateway_core::task::{DaemonTask, WorkerTaskError};
use tokio::sync::{Mutex, mpsc};

const DEFAULT_QUEUE_CAPACITY: usize = 4_096;

/// 准入读取保持强一致；终态释放只做有界入队，Redis 失败由 worker 吸收。
#[derive(Clone)]
pub struct BufferedClientAdmissionPort {
    inner: Arc<dyn ClientAdmissionPort>,
    sender: mpsc::Sender<AdmissionRelease>,
}

impl BufferedClientAdmissionPort {
    #[must_use]
    pub fn new(inner: Arc<dyn ClientAdmissionPort>) -> (Self, ClientAdmissionReleaseWriter) {
        Self::with_capacity(
            inner,
            NonZeroUsize::new(DEFAULT_QUEUE_CAPACITY).expect("queue capacity is non-zero"),
        )
    }

    #[must_use]
    pub fn with_capacity(
        inner: Arc<dyn ClientAdmissionPort>,
        capacity: NonZeroUsize,
    ) -> (Self, ClientAdmissionReleaseWriter) {
        let (sender, receiver) = mpsc::channel(capacity.get());
        (
            Self {
                inner: Arc::clone(&inner),
                sender,
            },
            ClientAdmissionReleaseWriter {
                inner,
                receiver: Mutex::new(receiver),
            },
        )
    }

    fn enqueue(&self, release: AdmissionRelease) -> bool {
        match self.sender.try_send(release) {
            Ok(()) => true,
            Err(error) => {
                let reason = match error {
                    mpsc::error::TrySendError::Full(_) => "full",
                    mpsc::error::TrySendError::Closed(_) => "closed",
                };
                tracing::warn!(
                    operation = "release_client_admission",
                    reason,
                    "Redis 协调队列不可用，已丢弃本次准入释放；租约将由 TTL 收敛"
                );
                false
            }
        }
    }
}

impl ClientAdmissionPort for BufferedClientAdmissionPort {
    fn admit(
        &self,
        request: ClientAdmissionRequest,
    ) -> BoxFuture<'_, Result<ClientAdmissionDecision, ClientAdmissionError>> {
        self.inner.admit(request)
    }

    fn release<'a>(
        &'a self,
        client_api_key_id: &'a ClientApiKeyId,
        model_request_id: &'a ModelRequestId,
    ) -> BoxFuture<'a, Result<bool, ClientAdmissionError>> {
        let enqueued = self.enqueue(AdmissionRelease {
            client_api_key_id: client_api_key_id.clone(),
            model_request_id: model_request_id.clone(),
        });
        Box::pin(ready(Ok(enqueued)))
    }

    fn restore(
        &self,
        recovery: ClientAdmissionRecovery,
    ) -> BoxFuture<'_, Result<ClientAdmissionRestoreResult, ClientAdmissionError>> {
        self.inner.restore(recovery)
    }
}

struct AdmissionRelease {
    client_api_key_id: ClientApiKeyId,
    model_request_id: ModelRequestId,
}

pub struct ClientAdmissionReleaseWriter {
    inner: Arc<dyn ClientAdmissionPort>,
    receiver: Mutex<mpsc::Receiver<AdmissionRelease>>,
}

impl DaemonTask for ClientAdmissionReleaseWriter {
    fn run(&self, cancellation: CancellationToken) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let mut receiver = self.receiver.lock().await;
            loop {
                let release = tokio::select! {
                    () = cancellation.cancelled() => return Ok(()),
                    release = receiver.recv() => release,
                };
                let Some(release) = release else {
                    return Err(WorkerTaskError::safe(
                        "client admission release queue closed",
                    ));
                };
                if let Err(error) = self
                    .inner
                    .release(&release.client_api_key_id, &release.model_request_id)
                    .await
                {
                    tracing::warn!(%error, "Client admission 后台释放失败，依赖租约 TTL 收敛");
                }
            }
        })
    }
}

/// Circuit decision 仍直读 Redis；success/failure feedback 只做有界入队。
#[derive(Clone)]
pub struct BufferedProviderCircuitPort {
    inner: Arc<dyn ProviderCircuitPort>,
    sender: mpsc::Sender<CircuitFeedback>,
}

impl BufferedProviderCircuitPort {
    #[must_use]
    pub fn new(inner: Arc<dyn ProviderCircuitPort>) -> (Self, ProviderCircuitFeedbackWriter) {
        Self::with_capacity(
            inner,
            NonZeroUsize::new(DEFAULT_QUEUE_CAPACITY).expect("queue capacity is non-zero"),
        )
    }

    #[must_use]
    pub fn with_capacity(
        inner: Arc<dyn ProviderCircuitPort>,
        capacity: NonZeroUsize,
    ) -> (Self, ProviderCircuitFeedbackWriter) {
        let (sender, receiver) = mpsc::channel(capacity.get());
        (
            Self {
                inner: Arc::clone(&inner),
                sender,
            },
            ProviderCircuitFeedbackWriter {
                inner,
                receiver: Mutex::new(receiver),
            },
        )
    }

    fn enqueue(&self, feedback: CircuitFeedback) {
        if let Err(error) = self.sender.try_send(feedback) {
            let reason = match error {
                mpsc::error::TrySendError::Full(_) => "full",
                mpsc::error::TrySendError::Closed(_) => "closed",
            };
            tracing::warn!(
                operation = "record_provider_circuit_feedback",
                reason,
                "Redis 协调队列不可用，已丢弃本次 circuit feedback"
            );
        }
    }
}

impl ProviderCircuitPort for BufferedProviderCircuitPort {
    fn decision<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
    ) -> BoxFuture<'a, Result<ProviderCircuitDecision, ProviderCircuitError>> {
        self.inner.decision(provider_kind)
    }

    fn observe_failure<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
    ) -> BoxFuture<'a, Result<(), ProviderCircuitError>> {
        self.enqueue(CircuitFeedback::Failure(provider_kind.clone()));
        Box::pin(ready(Ok(())))
    }

    fn observe_success<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
    ) -> BoxFuture<'a, Result<(), ProviderCircuitError>> {
        self.enqueue(CircuitFeedback::Success(provider_kind.clone()));
        Box::pin(ready(Ok(())))
    }
}

enum CircuitFeedback {
    Failure(ProviderKind),
    Success(ProviderKind),
}

pub struct ProviderCircuitFeedbackWriter {
    inner: Arc<dyn ProviderCircuitPort>,
    receiver: Mutex<mpsc::Receiver<CircuitFeedback>>,
}

impl DaemonTask for ProviderCircuitFeedbackWriter {
    fn run(&self, cancellation: CancellationToken) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let mut receiver = self.receiver.lock().await;
            loop {
                let feedback = tokio::select! {
                    () = cancellation.cancelled() => return Ok(()),
                    feedback = receiver.recv() => feedback,
                };
                let Some(feedback) = feedback else {
                    return Err(WorkerTaskError::safe(
                        "provider circuit feedback queue closed",
                    ));
                };
                let result = match feedback {
                    CircuitFeedback::Failure(provider) => {
                        self.inner.observe_failure(&provider).await
                    }
                    CircuitFeedback::Success(provider) => {
                        self.inner.observe_success(&provider).await
                    }
                };
                if let Err(error) = result {
                    tracing::warn!(%error, "Provider circuit feedback 后台写入失败");
                }
            }
        })
    }
}
