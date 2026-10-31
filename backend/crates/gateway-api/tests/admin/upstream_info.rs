//! 账号 API 通过公共互联端口读观测与提交刷新，不由具体 Provider 执行

use super::*;
use gateway_admin::model::provider_credentials::{ProviderQuota, ProviderQuotaReadRequest};
use gateway_admin::ports::peer::PeerUpstreamService;
use gateway_core::account::{ProviderAccount, UpstreamInfoSource, UpstreamSchedulingSignal};
use gateway_core::error::StoreError;
use std::time::SystemTime;

pub(super) struct TestPeerInfo(pub Arc<Mutex<Option<ProviderQuota>>>);

impl UpstreamInfoSource for TestPeerInfo {
    fn signal(
        &self,
        _: &ProviderAccount,
        _: &str,
        _: SystemTime,
    ) -> Option<UpstreamSchedulingSignal> {
        None
    }
}

#[async_trait]
impl PeerUpstreamService for TestPeerInfo {
    async fn account_changed(&self, _: &ProviderAccountId) -> Result<(), StoreError> {
        Ok(())
    }
    fn forget(&self, _: &ProviderAccountId) {}
    async fn request_refresh(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<gateway_admin::model::upstream_info::UpstreamRefreshReceipt, StoreError> {
        Ok(
            gateway_admin::model::upstream_info::UpstreamRefreshReceipt {
                account_id: account_id.as_str().to_owned(),
                connection_revision: "connection-7".to_owned(),
                request_generation: 7,
                state: gateway_admin::model::upstream_info::UpstreamSyncState::Queued,
            },
        )
    }
    async fn read_quota(&self, _: &ProviderAccountId) -> Result<Option<ProviderQuota>, StoreError> {
        Ok(self.0.lock().unwrap().clone())
    }
    async fn read_summary(
        &self,
        _: &ProviderAccountId,
    ) -> Result<Option<ProviderQuota>, StoreError> {
        Ok(self.0.lock().unwrap().clone())
    }
    async fn read_quotas(
        &self,
        requests: &[ProviderQuotaReadRequest<'_>],
    ) -> Vec<Result<ProviderQuota, ProviderAdminError>> {
        let quota = self.0.lock().unwrap().clone().unwrap_or_default();
        requests.iter().map(|_| Ok(quota.clone())).collect()
    }
}
