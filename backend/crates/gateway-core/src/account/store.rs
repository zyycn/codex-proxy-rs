//! Provider 账号持久化端口

use async_trait::async_trait;

use crate::error::StoreError;
use crate::identity::ProviderKind;

use super::{
    AccountStateChange, CredentialCasOutcome, CredentialCasUpdate, CredentialRevision,
    LoadedCredential, NewProviderAccount, ProviderAccount, ProviderAccountId,
    ProviderAccountUpdate, ProviderRefreshQuery, QuotaAccessChange, QuotaObservation,
    QuotaObservationTouch, QuotaObservationVersion, QuotaWriteOutcome,
};

/// `provider_accounts` 的数据库中立端口
#[async_trait]
pub trait ProviderAccountStore: Send + Sync {
    async fn create_account(&self, account: NewProviderAccount) -> Result<(), StoreError>;

    async fn get_account(
        &self,
        account: &ProviderAccountId,
    ) -> Result<Option<ProviderAccount>, StoreError>;

    async fn list_accounts(&self) -> Result<Vec<ProviderAccount>, StoreError>;

    async fn list_for_provider(
        &self,
        provider: &ProviderKind,
    ) -> Result<Vec<ProviderAccount>, StoreError>;

    /// 只读取指定 Provider 冻结范围内的已启用账号，空范围直接返回空池
    async fn list_for_provider_accounts(
        &self,
        provider: &ProviderKind,
        account_ids: &[ProviderAccountId],
    ) -> Result<Vec<ProviderAccount>, StoreError>;

    /// 一次有界查询返回账号事实和 revision-fenced 明文 credential
    async fn list_refresh_candidates(
        &self,
        query: ProviderRefreshQuery,
    ) -> Result<Vec<LoadedCredential>, StoreError>;

    async fn load_credential(
        &self,
        account: &ProviderAccountId,
        expected_revision: CredentialRevision,
    ) -> Result<LoadedCredential, StoreError>;

    /// 读取账号当前 credential 及其 revision，不做任何版本比对
    ///
    /// 管理写入必须在临近 CAS 时用它取 fence，而不是携带调用方持有的旧 revision：
    /// 后台刷新随时会推进 revision，用陈旧快照会把正常的恢复操作误判为冲突
    async fn load_current_credential(
        &self,
        account: &ProviderAccountId,
    ) -> Result<LoadedCredential, StoreError>;

    async fn compare_and_swap_credential(
        &self,
        update: CredentialCasUpdate,
    ) -> Result<CredentialCasOutcome, StoreError>;

    async fn get_quotas(
        &self,
        accounts: &[ProviderAccountId],
    ) -> Result<Vec<QuotaObservation>, StoreError>;

    /// 仅查询快照版本，列表缓存校验不物化额度正文
    async fn get_quota_versions(
        &self,
        accounts: &[ProviderAccountId],
    ) -> Result<Vec<QuotaObservationVersion>, StoreError> {
        Ok(self
            .get_quotas(accounts)
            .await?
            .into_iter()
            .map(|quota| QuotaObservationVersion {
                account_id: quota.account_id,
                credential_revision: quota.expected_revision,
                observed_at: quota.observed_at,
            })
            .collect())
    }

    async fn compare_and_swap_quota(
        &self,
        observation: QuotaObservation,
    ) -> Result<QuotaWriteOutcome, StoreError>;

    async fn touch_quota_observation(
        &self,
        touch: QuotaObservationTouch,
    ) -> Result<QuotaWriteOutcome, StoreError>;

    async fn apply_quota_access(
        &self,
        change: QuotaAccessChange,
    ) -> Result<QuotaWriteOutcome, StoreError>;

    async fn apply_state_change(&self, change: AccountStateChange) -> Result<(), StoreError>;

    async fn update_account(&self, update: ProviderAccountUpdate) -> Result<(), StoreError>;

    async fn set_enabled(
        &self,
        account: &ProviderAccountId,
        enabled: bool,
    ) -> Result<(), StoreError>;

    async fn delete_account(&self, account: &ProviderAccountId) -> Result<(), StoreError>;
}
