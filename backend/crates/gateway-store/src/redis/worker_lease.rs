//! 多实例 worker leader lease 的 Redis 实现。

use std::num::NonZeroU64;

use gateway_core::task::{
    WorkerFencingToken, WorkerLeaderLeaseGuard, WorkerLeaderLeasePort, WorkerLeaseAcquisition,
    WorkerLeaseError, WorkerLeaseRequest,
};
use uuid::Uuid;

use super::{
    CredentialLeaseGrant, CredentialLeaseRepository, CredentialLeaseRequest, CredentialLeaseScope,
    RedisCredentialLeaseRepository,
};

pub(crate) struct RedisWorkerLeaderLeasePort {
    repository: RedisCredentialLeaseRepository,
    owner_id: String,
}

impl RedisWorkerLeaderLeasePort {
    #[must_use]
    pub(crate) fn new(repository: RedisCredentialLeaseRepository) -> Self {
        Self {
            repository,
            owner_id: format!("worker-process-{}", Uuid::now_v7().simple()),
        }
    }
}

struct RedisWorkerLeaderLeaseGuard {
    repository: RedisCredentialLeaseRepository,
    request: CredentialLeaseRequest,
    grant: Option<CredentialLeaseGrant>,
    token: WorkerFencingToken,
}

impl WorkerLeaderLeaseGuard for RedisWorkerLeaderLeaseGuard {
    fn fencing_token(&self) -> WorkerFencingToken {
        self.token
    }

    fn renew(&mut self) -> futures::future::BoxFuture<'_, Result<(), WorkerLeaseError>> {
        Box::pin(async move {
            let current = self
                .grant
                .as_ref()
                .ok_or_else(|| WorkerLeaseError::safe("worker lease is released"))?;
            let renewed = CredentialLeaseRepository::renew_credential_lease(
                &self.repository,
                &self.request,
                current,
            )
            .await
            .map_err(|_| WorkerLeaseError::safe("worker lease renewal failed"))?
            .ok_or_else(|| WorkerLeaseError::safe("worker lease was lost"))?;
            self.grant = Some(renewed);
            Ok(())
        })
    }

    fn release(
        mut self: Box<Self>,
    ) -> futures::future::BoxFuture<'static, Result<(), WorkerLeaseError>> {
        let repository = self.repository.clone();
        let request = self.request.clone();
        let grant = self.grant.take();
        Box::pin(async move {
            let Some(grant) = grant else {
                return Ok(());
            };
            CredentialLeaseRepository::release_credential_lease(&repository, &request, &grant)
                .await
                .map(|_| ())
                .map_err(|_| WorkerLeaseError::safe("worker lease release failed"))
        })
    }
}

impl WorkerLeaderLeasePort for RedisWorkerLeaderLeasePort {
    fn try_acquire(
        &self,
        request: WorkerLeaseRequest,
    ) -> futures::future::BoxFuture<'_, Result<WorkerLeaseAcquisition, WorkerLeaseError>> {
        Box::pin(async move {
            let lease_request = CredentialLeaseRequest {
                scope: CredentialLeaseScope::ProviderTask,
                resource_id: request.worker().to_string(),
                owner_id: self.owner_id.clone(),
                ttl: request.ttl(),
            };
            let grant = CredentialLeaseRepository::acquire_credential_lease(
                &self.repository,
                &lease_request,
            )
            .await
            .map_err(|_| WorkerLeaseError::safe("worker lease acquisition failed"))?;
            let Some(grant) = grant else {
                return Ok(WorkerLeaseAcquisition::Busy { retry_after: None });
            };
            let token = NonZeroU64::new(grant.fencing_token.get())
                .map(WorkerFencingToken::new)
                .ok_or_else(|| WorkerLeaseError::safe("worker fencing token is invalid"))?;
            Ok(WorkerLeaseAcquisition::Acquired(Box::new(
                RedisWorkerLeaderLeaseGuard {
                    repository: self.repository.clone(),
                    request: lease_request,
                    grant: Some(grant),
                    token,
                },
            )))
        })
    }
}
