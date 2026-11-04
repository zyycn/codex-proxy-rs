//! CPR 连接材料与传输端口；Provider 解释凭据，Host 执行受限网络请求

use std::time::Duration;

use async_trait::async_trait;
use gateway_core::account::{ProviderAccount, UpstreamInfoFailure};
use secrecy::SecretString;

/// 仅供后台请求使用，不进入日志、持久观测或管理响应
pub struct PeerConnection {
    pub base_url: String,
    pub bearer_token: SecretString,
}

pub struct PeerFetchFailure {
    pub kind: UpstreamInfoFailure,
    pub retry_after: Option<Duration>,
}

impl From<UpstreamInfoFailure> for PeerFetchFailure {
    fn from(kind: UpstreamInfoFailure) -> Self {
        Self {
            kind,
            retry_after: None,
        }
    }
}

#[async_trait]
pub trait PeerTransport: Send + Sync {
    async fn fetch(
        &self,
        account: &ProviderAccount,
        connection: &PeerConnection,
    ) -> Result<Vec<u8>, PeerFetchFailure>;
}

/// 账号用例读取公共互联观测，显式刷新等待额度查询完成
#[async_trait]
pub trait PeerUpstreamService: gateway_core::account::UpstreamInfoSource {
    /// 仅在指定持久观测与当前调度缓存一致时估算容量
    fn observed_concurrency_limit(
        &self,
        version: &gateway_core::account::QuotaObservationVersion,
        now: std::time::SystemTime,
    ) -> Option<gateway_core::account::AccountConcurrencyLimit>;

    async fn account_changed(
        &self,
        id: &gateway_core::account::ProviderAccountId,
    ) -> Result<(), gateway_core::error::StoreError>;
    fn forget(&self, id: &gateway_core::account::ProviderAccountId);
    async fn refresh_quota(
        &self,
        id: &gateway_core::account::ProviderAccountId,
    ) -> Result<
        crate::model::provider_credentials::ProviderQuota,
        super::provider::ProviderAdminError,
    >;
    async fn read_quota(
        &self,
        account_id: &gateway_core::account::ProviderAccountId,
    ) -> Result<
        Option<crate::model::provider_credentials::ProviderQuota>,
        gateway_core::error::StoreError,
    >;
    /// 读取版本化摘要，不展开远端节点窗口
    async fn read_summary(
        &self,
        account_id: &gateway_core::account::ProviderAccountId,
    ) -> Result<
        Option<crate::model::provider_credentials::ProviderQuota>,
        gateway_core::error::StoreError,
    >;
    async fn read_quotas(
        &self,
        requests: &[crate::model::provider_credentials::ProviderQuotaReadRequest<'_>],
    ) -> Vec<
        Result<
            crate::model::provider_credentials::ProviderQuota,
            super::provider::ProviderAdminError,
        >,
    >;
}
