//! Client Key 与 Provider 账号配对的持久 installation id 合同

use super::ProviderStoreError;
use crate::{account::ProviderAccountId, policy::ClientApiKeyId};
use futures::future::BoxFuture;
use uuid::Uuid;

/// installation id 使用稳定记录 ID，不依赖客户端密钥或请求画像
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderClientInstallationKey {
    client_api_key_id: ClientApiKeyId,
    provider_account_id: ProviderAccountId,
}

impl ProviderClientInstallationKey {
    #[must_use]
    pub const fn new(
        client_api_key_id: ClientApiKeyId,
        provider_account_id: ProviderAccountId,
    ) -> Self {
        Self {
            client_api_key_id,
            provider_account_id,
        }
    }

    #[must_use]
    pub const fn client_api_key_id(&self) -> &ClientApiKeyId {
        &self.client_api_key_id
    }

    #[must_use]
    pub const fn provider_account_id(&self) -> &ProviderAccountId {
        &self.provider_account_id
    }
}

/// 只持久化配对 UUID；启用策略与缓存由 Provider 拥有
pub trait ProviderClientInstallationPort: Send + Sync {
    fn read<'a>(
        &'a self,
        key: &'a ProviderClientInstallationKey,
    ) -> BoxFuture<'a, Result<Option<Uuid>, ProviderStoreError>>;

    /// 返回已提交的 UUID；并发创建时保留并返回先提交的值
    fn get_or_insert<'a>(
        &'a self,
        key: &'a ProviderClientInstallationKey,
        candidate: Uuid,
    ) -> BoxFuture<'a, Result<Uuid, ProviderStoreError>>;
}
