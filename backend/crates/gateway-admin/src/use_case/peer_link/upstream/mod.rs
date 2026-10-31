//! 公共 CPR 上游同步、管理投影与调度观测，连接凭据解释由 Provider 提供

mod accounts;
mod projection;
mod scheduling;
mod service;
mod snapshot;
mod worker;

use std::sync::Arc;

use crate::{
    model::{provider_credentials::ProviderQuota, upstream_info::UpstreamRefreshReceipt},
    ports::{
        peer::{PeerTransport, PeerUpstreamService},
        provider::ProviderAdminRegistry,
    },
};
pub(crate) use accounts::{read_quota, read_quotas, store_error};
use gateway_core::{
    account::{ProviderAccountId, ProviderAccountStore, UpstreamInfoSource, UpstreamInfoStore},
    error::StoreError,
    task::{WorkerContribution, WorkerDefinitionError},
};
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
    store: Arc<dyn UpstreamInfoStore>,
    providers: &ProviderAdminRegistry,
    transport: Arc<dyn PeerTransport>,
) -> Result<PeerUpstreamBundle, WorkerDefinitionError> {
    let service = Arc::new(PeerInfoService::new(accounts, store, providers, transport));
    let worker = service.worker()?;
    Ok(PeerUpstreamBundle {
        service,
        workers: vec![worker],
    })
}

#[async_trait::async_trait]
impl PeerUpstreamService for PeerInfoService {
    async fn account_changed(&self, id: &ProviderAccountId) -> Result<(), StoreError> {
        PeerInfoService::account_changed(self, id).await
    }

    fn forget(&self, id: &ProviderAccountId) {
        PeerInfoService::forget(self, id);
    }

    async fn request_refresh(
        &self,
        id: &ProviderAccountId,
    ) -> Result<UpstreamRefreshReceipt, StoreError> {
        PeerInfoService::request_refresh(self, id)
            .await
            .map(|plan| Self::receipt(&plan))
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
