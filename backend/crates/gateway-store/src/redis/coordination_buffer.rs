//! 可由 TTL 或后续反馈收敛的 Redis 写副作用队列
//!
//! Continuation affinity 决定下一轮 Provider 与账号，必须在响应 ID 可复用前
//! 获得 Redis 确认，因此不属于本模块的可丢失副作用

use std::sync::Arc;
use std::{
    num::NonZeroUsize,
    time::{Duration, Instant},
};

use futures::future::{BoxFuture, ready};
use gateway_core::engine::ModelRequestId;
use gateway_core::engine::admission::{
    ClientAdmissionDecision, ClientAdmissionError, ClientAdmissionPort, ClientAdmissionRecovery,
    ClientAdmissionRequest, ClientAdmissionRestoreResult,
};
use gateway_core::lifecycle::CancellationToken;
use gateway_core::policy::ClientApiKeyId;
use gateway_core::task::{DaemonTask, WorkerDaemonContext, WorkerTaskError};
use tokio::sync::{Mutex, mpsc};

const DEFAULT_QUEUE_CAPACITY: usize = 4_096;
const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// 准入读取保持强一致；终态释放只做有界入队，Redis 失败由 worker 吸收
#[derive(Clone)]
pub struct BufferedClientAdmissionPort {
    inner: Arc<dyn ClientAdmissionPort>,
    sender: mpsc::Sender<AdmissionRelease>,
}

impl BufferedClientAdmissionPort {
    #[must_use]
    pub fn new(
        inner: Arc<dyn ClientAdmissionPort>,
        diagnostics: Arc<dyn gateway_core::diagnostics::OperationalDiagnostics>,
    ) -> (Self, ClientAdmissionReleaseWriter) {
        Self::with_capacity(
            inner,
            NonZeroUsize::new(DEFAULT_QUEUE_CAPACITY).expect("queue capacity is non-zero"),
            diagnostics,
        )
    }

    #[must_use]
    pub fn with_capacity(
        inner: Arc<dyn ClientAdmissionPort>,
        capacity: NonZeroUsize,
        diagnostics: Arc<dyn gateway_core::diagnostics::OperationalDiagnostics>,
    ) -> (Self, ClientAdmissionReleaseWriter) {
        let (sender, receiver) = mpsc::channel(capacity.get());
        (
            Self {
                inner: Arc::clone(&inner),
                sender,
            },
            ClientAdmissionReleaseWriter {
                inner,
                diagnostics,
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
    fn maintain(
        &self,
        key: &ClientApiKeyId,
        request: &ModelRequestId,
        deadline: gateway_core::lifecycle::Deadline,
        cancellation: CancellationToken,
    ) -> Box<dyn gateway_core::lifecycle::LeaseGuard> {
        self.inner.maintain(key, request, deadline, cancellation)
    }

    fn abandon(&self, key: &ClientApiKeyId, request: &ModelRequestId) {
        self.enqueue(AdmissionRelease {
            client_api_key_id: key.clone(),
            model_request_id: request.clone(),
        });
    }

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
    diagnostics: Arc<dyn gateway_core::diagnostics::OperationalDiagnostics>,
    inner: Arc<dyn ClientAdmissionPort>,
    receiver: Mutex<mpsc::Receiver<AdmissionRelease>>,
}

impl DaemonTask for ClientAdmissionReleaseWriter {
    fn run(&self, context: WorkerDaemonContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let cancellation = context.cancellation();
            let mut receiver = self.receiver.lock().await;
            loop {
                let release = tokio::select! {
                    () = cancellation.cancelled() => {
                        drain_admission_releases(&mut receiver, self.inner.as_ref(), self.diagnostics.as_ref()).await;
                        return Ok(());
                    },
                    release = receiver.recv() => release,
                };
                let Some(release) = release else {
                    return Err(WorkerTaskError::safe(
                        "client admission release queue closed",
                    ));
                };
                if release_admission(self.inner.as_ref(), self.diagnostics.as_ref(), release).await
                {
                    context.succeeded();
                }
            }
        })
    }
}

async fn drain_admission_releases(
    receiver: &mut mpsc::Receiver<AdmissionRelease>,
    port: &dyn ClientAdmissionPort,
    diagnostics: &dyn gateway_core::diagnostics::OperationalDiagnostics,
) {
    receiver.close();
    let started_at = Instant::now();
    while let Some(release) = receiver.recv().await {
        let remaining = SHUTDOWN_DRAIN_TIMEOUT.saturating_sub(started_at.elapsed());
        if remaining.is_zero()
            || tokio::time::timeout(remaining, release_admission(port, diagnostics, release))
                .await
                .is_err()
        {
            tracing::warn!("Client admission 关闭排空超时，剩余租约依赖 TTL 收敛");
            break;
        }
    }
}

async fn release_admission(
    port: &dyn ClientAdmissionPort,
    diagnostics: &dyn gateway_core::diagnostics::OperationalDiagnostics,
    release: AdmissionRelease,
) -> bool {
    if let Err(source) = port
        .release(&release.client_api_key_id, &release.model_request_id)
        .await
    {
        let mut failure = gateway_core::diagnostics::OperationalFailure::new(
            "store",
            "release_client_admission",
            "admission_unavailable",
            "Client admission release failed; lease TTL remains active",
        );
        failure.correlation_id = Some(release.model_request_id.as_str().to_owned());
        failure.details =
            gateway_core::error::ErrorDetails::capture(Some(&source.into()), None, false);
        if diagnostics.record_failure(failure).await.is_err() {
            tracing::warn!(
                request_id = release.model_request_id.as_str(),
                "admission diagnostic could not be recorded"
            );
        }
        return false;
    }
    true
}
