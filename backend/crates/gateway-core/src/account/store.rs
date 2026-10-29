//! Provider 账号持久化端口

use async_trait::async_trait;
use std::time::{Duration, SystemTime};

use crate::error::StoreError;
use crate::identity::ProviderKind;

use super::{
    AccountStateChange, CredentialCasOutcome, CredentialCasUpdate, CredentialRevision,
    LoadedCredential, NewProviderAccount, ProviderAccount, ProviderAccountId,
    ProviderAccountUpdate, ProviderRefreshQuery, QuotaAccessChange, QuotaObservation,
    QuotaObservationTouch, QuotaWriteOutcome, UpstreamConnection, UpstreamInfoClaim,
    UpstreamInfoCompletion, UpstreamInfoPlan, UpstreamInfoRecord,
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

/// 与 Provider 无关的计划持久化端口，所有方法均只访问本地存储
#[async_trait]
pub trait UpstreamInfoStore: Send + Sync {
    /// 按账号 ID 游标扫描 API Key 账号；是否启用互联仍由 Provider 解释凭据
    async fn list_candidates(
        &self,
        provider: &ProviderKind,
        after: Option<&ProviderAccountId>,
        limit: u32,
    ) -> Result<Vec<LoadedCredential>, StoreError>;

    /// 新连接创建立即到期的计划；身份变化清空旧观测；停用则删除计划与观测
    /// 写入前须按当前账号事实核对连接身份；匹配失败返回 Conflict
    async fn reconcile(
        &self,
        connection: &UpstreamConnection,
        enabled: bool,
        now: SystemTime,
    ) -> Result<(), StoreError>;

    /// 请求代次合并；正在领取或排队的计划不重复递增，失败退避不能被手动绕过
    async fn request_refresh(
        &self,
        connection: &UpstreamConnection,
        now: SystemTime,
        minimum_interval: Duration,
    ) -> Result<UpstreamInfoPlan, StoreError>;

    /// 批量读取同步元数据，用于核对缓存观测版本，不读取快照正文
    async fn read_plans(
        &self,
        accounts: &[ProviderAccountId],
    ) -> Result<Vec<UpstreamInfoPlan>, StoreError>;

    async fn read(
        &self,
        accounts: &[ProviderAccountId],
    ) -> Result<Vec<UpstreamInfoRecord>, StoreError>;

    /// 原子领取到期计划并设置租期；每次领取生成不能复用的领取标识
    async fn claim_due(
        &self,
        provider: &ProviderKind,
        now: SystemTime,
        limit: u32,
        lease_duration: Duration,
    ) -> Result<Vec<UpstreamInfoClaim>, StoreError>;

    /// 返回 false 代表连接、领取标识或代次已经变化；Unauthorized 必须同时清空观测
    async fn complete(&self, completion: UpstreamInfoCompletion) -> Result<bool, StoreError>;
}
