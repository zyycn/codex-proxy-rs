//! 将互联同步交给 Host 周期监督并响应取消

use std::{sync::Arc, time::Duration};

use futures::future::BoxFuture;
use gateway_core::task::{
    ScheduledTask, WorkerContribution, WorkerCycleContext, WorkerDefinitionError, WorkerId,
    WorkerKind, WorkerLeaseRequest, WorkerRegistration, WorkerRunnable, WorkerSchedule,
    WorkerTaskError,
};

use super::PeerInfoService;

impl PeerInfoService {
    pub(crate) fn worker(self: &Arc<Self>) -> Result<WorkerContribution, WorkerDefinitionError> {
        let id = WorkerId::try_new(WorkerKind::QuotaCatalogHealth, "openai-peer-info")?;
        let ttl = Duration::from_secs(120);
        let schedule = WorkerSchedule::try_new(
            Duration::from_secs(15),
            Duration::from_secs(1),
            Duration::from_secs(60),
            ttl,
            Duration::from_secs(30),
        )?;
        let lease = WorkerLeaseRequest::try_new(id.clone(), ttl)?;
        Ok(WorkerContribution::Registration(
            WorkerRegistration::try_new(
                id,
                WorkerRunnable::Scheduled {
                    schedule,
                    lease: Some(lease),
                    task: Box::new(PeerInfoTask(Arc::clone(self))),
                },
            )?,
        ))
    }
}

struct PeerInfoTask(Arc<PeerInfoService>);

impl ScheduledTask for PeerInfoTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            tokio::select! {
                () = context.cancellation().cancelled() => Ok(()),
                result = self.0.synchronize(&context) => result.map_err(|error| WorkerTaskError::safe("upstream information synchronization failed").with_source(error)),
            }
        })
    }
}
