//! 公共 CPR 上游同步、管理投影与调度观测，连接凭据解释由 Provider 提供

mod accounts;
mod cache;
mod projection;
mod quota;
mod refresh;
mod scheduling;
mod service;
mod snapshot;
mod worker;

use std::sync::Arc;

use crate::{
    model::provider_credentials::ProviderQuota,
    ports::{
        peer::{PeerTransport, PeerUpstreamService},
        provider::ProviderAdminRegistry,
    },
};
pub(crate) use accounts::{read_quota, read_quotas};
use gateway_core::{
    account::{ProviderAccountId, ProviderAccountStore, UpstreamInfoSource},
    error::StoreError,
    task::{WorkerContribution, WorkerDefinitionError},
};
pub(crate) use projection::account_capacity;
use service::PeerInfoService;

/// 组合根独立注册公共互联 Worker，不属于任何具体 Provider 的任务集合
pub struct PeerUpstreamBundle {
    service: Arc<PeerInfoService>,
    workers: Vec<WorkerContribution>,
}

impl PeerUpstreamBundle {
    pub fn service(&self) -> Arc<dyn PeerUpstreamService> {
        self.service.clone()
    }

    pub fn scheduling_source(&self) -> Arc<dyn UpstreamInfoSource> {
        self.service.clone()
    }

    pub fn take_worker_contributions(&mut self) -> Vec<WorkerContribution> {
        std::mem::take(&mut self.workers)
    }
}

pub fn initialize(
    accounts: Arc<dyn ProviderAccountStore>,
    providers: &ProviderAdminRegistry,
    transport: Arc<dyn PeerTransport>,
) -> Result<PeerUpstreamBundle, WorkerDefinitionError> {
    let service = Arc::new(PeerInfoService::new(accounts, providers, transport));
    let worker = service.worker()?;
    Ok(PeerUpstreamBundle {
        service,
        workers: vec![worker],
    })
}

#[async_trait::async_trait]
impl PeerUpstreamService for PeerInfoService {
    fn observed_concurrency_limit(
        &self,
        version: &gateway_core::account::QuotaObservationVersion,
        now: std::time::SystemTime,
    ) -> Option<gateway_core::account::AccountConcurrencyLimit> {
        let observations = self
            .observations
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cached = observations.get(&version.account_id)?;
        if cached.observed_at != Some(version.observed_at)
            || cached.connection.credential_revision != version.credential_revision
        {
            return None;
        }
        cached.scheduling.as_ref()?.concurrency_limit(now)
    }

    async fn account_changed(&self, id: &ProviderAccountId) -> Result<(), StoreError> {
        PeerInfoService::account_changed(self, id).await
    }

    fn forget(&self, id: &ProviderAccountId) {
        PeerInfoService::forget(self, id);
    }

    async fn refresh_quota(
        &self,
        id: &ProviderAccountId,
    ) -> Result<ProviderQuota, crate::ports::provider::ProviderAdminError> {
        PeerInfoService::refresh(self, id, true).await
    }

    async fn read_quota(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<Option<ProviderQuota>, StoreError> {
        PeerInfoService::read_quota(self, account_id).await
    }

    async fn read_summary(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<Option<ProviderQuota>, StoreError> {
        PeerInfoService::read_summary(self, account_id).await
    }

    async fn read_quotas(
        &self,
        requests: &[crate::model::provider_credentials::ProviderQuotaReadRequest<'_>],
    ) -> Vec<Result<ProviderQuota, crate::ports::provider::ProviderAdminError>> {
        accounts::read_peer_quotas(self, requests).await
    }
}
