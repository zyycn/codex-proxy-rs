//! 费用登记与后台落库分离，准入同时检查尚未落库的精确费用

use std::{
    collections::{BTreeMap, VecDeque},
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use futures::future::BoxFuture;
use gateway_core::{
    diagnostics::{OperationalDiagnostics, OperationalFailure},
    engine::budget::{ClientBudgetCharge, ClientBudgetError, ClientBudgetPort},
    error::{ErrorDetails, GatewayError},
    policy::ClientApiKeyId,
    task::{DaemonTask, WorkerDaemonContext, WorkerTaskError},
};
use tokio::sync::Notify;

use super::{PgClientBudgetStore, store::unavailable};

const DEFAULT_PENDING_CAPACITY: NonZeroUsize =
    NonZeroUsize::new(4_096).expect("non-zero queue capacity");
const ADMISSION_TIMEOUT: Duration = Duration::from_secs(1);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(3);
const WRITE_BATCH_SIZE: usize = 32;

#[derive(Default)]
struct PendingCharges {
    by_key: BTreeMap<String, BTreeMap<String, ClientBudgetCharge>>,
    ready: VecDeque<(String, String)>,
    count: usize,
    closing: bool,
}

#[derive(Default)]
struct BudgetQueue {
    pending: Mutex<PendingCharges>,
    changed: Notify,
}

/// 单副本运行时先接管费用，再由受监督的 writer 落库和重试
pub struct BufferedClientBudgetStore {
    inner: Arc<PgClientBudgetStore>,
    queue: Arc<BudgetQueue>,
    capacity: NonZeroUsize,
}

impl BufferedClientBudgetStore {
    #[must_use]
    pub fn new(
        inner: PgClientBudgetStore,
        diagnostics: Arc<dyn OperationalDiagnostics>,
    ) -> (Self, ClientBudgetWriter) {
        Self::with_capacity(inner, DEFAULT_PENDING_CAPACITY, diagnostics)
    }

    #[must_use]
    pub fn with_capacity(
        inner: PgClientBudgetStore,
        capacity: NonZeroUsize,
        diagnostics: Arc<dyn OperationalDiagnostics>,
    ) -> (Self, ClientBudgetWriter) {
        let inner = Arc::new(inner);
        let queue = Arc::new(BudgetQueue::default());
        (
            Self {
                inner: inner.clone(),
                queue: queue.clone(),
                capacity,
            },
            ClientBudgetWriter {
                inner,
                queue,
                diagnostics,
                batch_limits: tokio::sync::Mutex::new(BTreeMap::new()),
            },
        )
    }
}

impl ClientBudgetPort for BufferedClientBudgetStore {
    fn admit(&self, key_id: ClientApiKeyId) -> BoxFuture<'_, Result<(), GatewayError>> {
        Box::pin(async move {
            let pending = {
                let state = self.queue.pending.lock().map_err(|_| unavailable())?;
                if state.closing || state.count >= self.capacity.get() {
                    return Err(unavailable());
                }
                state
                    .by_key
                    .get(key_id.as_str())
                    .map(|charges| charges.values().cloned().collect::<Vec<_>>())
                    .unwrap_or_default()
            };
            tokio::time::timeout(
                ADMISSION_TIMEOUT,
                self.inner.admit_with_pending(key_id, &pending),
            )
            .await
            .map_err(|source| unavailable().with_source(source))?
        })
    }

    fn settle(&self, charge: ClientBudgetCharge) -> BoxFuture<'_, Result<(), ClientBudgetError>> {
        Box::pin(async move {
            let mut state = self
                .queue
                .pending
                .lock()
                .map_err(|_| ClientBudgetError(None))?;
            let key = charge.key_id.as_str().to_owned();
            let request = charge.request_id.as_str().to_owned();
            let charges = state.by_key.entry(key.clone()).or_default();
            if charges.contains_key(&request) {
                return Ok(());
            }
            charges.insert(request.clone(), charge);
            state.ready.push_back((key, request));
            state.count += 1;
            // 达到容量后拒绝新准入，但已准入请求的费用必须接收，不能丢弃或反向等待写入
            self.queue.changed.notify_one();
            if state.closing {
                return Err(ClientBudgetError(None));
            }
            Ok(())
        })
    }
}

/// 每次仅占一个写入任务；任务重启、取消和超时后仍保留同一笔费用
pub struct ClientBudgetWriter {
    inner: Arc<PgClientBudgetStore>,
    queue: Arc<BudgetQueue>,
    diagnostics: Arc<dyn OperationalDiagnostics>,
    // 独占 writer，同时保留各 Key 跨任务重启的缩批进度
    batch_limits: tokio::sync::Mutex<BTreeMap<String, usize>>,
}

struct PendingWrite {
    queue: Arc<BudgetQueue>,
    key: String,
    charges: Vec<ClientBudgetCharge>,
}

impl PendingWrite {
    fn complete(&self) -> Result<bool, WorkerTaskError> {
        let mut state = self.queue.pending.lock().map_err(|_| queue_error())?;
        if let Some(charges) = state.by_key.get_mut(&self.key) {
            let before = charges.len();
            for charge in &self.charges {
                charges.remove(charge.request_id.as_str());
            }
            let removed = before - charges.len();
            let empty = charges.is_empty();
            state.count -= removed;
            if empty {
                state.by_key.remove(&self.key);
            }
        }
        Ok(!state.by_key.contains_key(&self.key))
    }
}

impl Drop for PendingWrite {
    fn drop(&mut self) {
        let Ok(mut state) = self.queue.pending.lock() else {
            return;
        };
        // 只有提交确认后才移除；future 被取消时重排队，由请求 ID 处理提交结果未知
        for charge in &self.charges {
            let request = charge.request_id.as_str();
            if state
                .by_key
                .get(&self.key)
                .is_some_and(|charges| charges.contains_key(request))
            {
                state
                    .ready
                    .push_back((self.key.clone(), request.to_owned()));
                self.queue.changed.notify_one();
            }
        }
    }
}

impl ClientBudgetWriter {
    fn next(
        &self,
        batch_limits: &BTreeMap<String, usize>,
    ) -> Result<Option<PendingWrite>, WorkerTaskError> {
        let mut state = self.queue.pending.lock().map_err(|_| queue_error())?;
        let Some((key, _)) = state.ready.front() else {
            return Ok(None);
        };
        let key = key.clone();
        let limit = batch_limits.get(&key).copied().unwrap_or(WRITE_BATCH_SIZE);
        let mut requests = Vec::with_capacity(limit);
        // 混合 Key 的请求也能合批；限制单批数量，并保持其他 Key 的相对顺序
        state.ready.retain(|(candidate, request)| {
            if candidate == &key && requests.len() < limit {
                requests.push(request.clone());
                false
            } else {
                true
            }
        });
        let charges = requests
            .iter()
            .map(|request| {
                state
                    .by_key
                    .get(&key)
                    .and_then(|charges| charges.get(request))
                    .cloned()
                    .ok_or_else(queue_error)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(PendingWrite {
            queue: self.queue.clone(),
            key,
            charges,
        }))
    }

    async fn write(
        &self,
        batch_limits: &mut BTreeMap<String, usize>,
        pending: PendingWrite,
    ) -> Result<(), WorkerTaskError> {
        let started = Instant::now();
        let result =
            match tokio::time::timeout(WRITE_TIMEOUT, self.inner.settle_charges(&pending.charges))
                .await
            {
                Ok(result) => result,
                Err(source) => Err(ClientBudgetError(Some(source.into()))),
            };
        if let Err(source) = result {
            // 失败后只缩小当前 Key，避免固定大批次持续超过写入时限
            batch_limits.insert(pending.key.clone(), (pending.charges.len() / 2).max(1));
            let mut failure = OperationalFailure::new(
                "store",
                "settle_client_budget",
                "budget_unavailable",
                "Client budget settlement failed; background writer will retry",
            );
            failure.correlation_id = pending
                .charges
                .first()
                .map(|charge| charge.request_id.as_str().to_owned());
            failure.details = ErrorDetails::capture(Some(&source.clone().into()), None, false);
            let _ = self.diagnostics.record_failure(failure).await;
            return Err(
                WorkerTaskError::safe("client budget settlement failed").with_source(source)
            );
        }
        if pending.complete()? {
            batch_limits.remove(&pending.key);
        } else if let Some(limit) = batch_limits.get_mut(&pending.key)
            && pending.charges.len() == *limit
            && started.elapsed() < WRITE_TIMEOUT / 4
        {
            // 满批写入足够快才恢复吞吐，保留余量避免慢库反复放大到超时
            *limit = (*limit * 2).min(WRITE_BATCH_SIZE);
            if *limit == WRITE_BATCH_SIZE {
                batch_limits.remove(&pending.key);
            }
        }
        Ok(())
    }

    async fn drain(
        &self,
        batch_limits: &mut BTreeMap<String, usize>,
    ) -> Result<(), WorkerTaskError> {
        self.queue
            .pending
            .lock()
            .map_err(|_| queue_error())?
            .closing = true;
        let result = tokio::time::timeout(DRAIN_TIMEOUT, async {
            while let Some(pending) = self.next(batch_limits)? {
                self.write(batch_limits, pending).await?;
            }
            Ok(())
        })
        .await;
        let result = result
            .map_err(|source| {
                WorkerTaskError::safe("client budget shutdown drain timed out").with_source(source)
            })
            .and_then(std::convert::identity);
        if let Err(error) = &result {
            tracing::warn!(operation = "settle_client_budget", %error, "费用队列关闭排空未完成");
        }
        result
    }
}

impl DaemonTask for ClientBudgetWriter {
    fn run(&self, context: WorkerDaemonContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let mut batch_limits = self.batch_limits.lock().await;
            loop {
                let write = async {
                    match self.next(&batch_limits)? {
                        Some(pending) => {
                            self.write(&mut batch_limits, pending).await?;
                            context.succeeded();
                        }
                        None => self.queue.changed.notified().await,
                    }
                    Ok::<(), WorkerTaskError>(())
                };
                tokio::select! {
                    biased;
                    () = context.cancellation().cancelled() => return self.drain(&mut batch_limits).await,
                    result = write => result?,
                }
            }
        })
    }
}

impl Drop for ClientBudgetWriter {
    fn drop(&mut self) {
        if let Ok(mut state) = self.queue.pending.lock() {
            state.closing = true;
            if state.count > 0 {
                tracing::warn!(
                    operation = "settle_client_budget",
                    pending_charges = state.count,
                    "费用写入任务已销毁，仍有未落库费用"
                );
            }
        }
    }
}

fn queue_error() -> WorkerTaskError {
    WorkerTaskError::safe("client budget settlement queue is unavailable")
}
