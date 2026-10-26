//! 插件查询账号、Key 与额度事实的类型化宿主客户端

use crate::{PluginFault, call::data};

use super::{HostClient, payload_call};

impl HostClient {
    /// 通过宿主入口认证计划校验凭据并取得执行身份，不执行模型或记录使用事实
    ///
    /// # Errors
    /// 凭据无效、认证失败或父调用取消时返回错误
    pub async fn verify_client(
        &self,
        request: data::VerifyClientRequest,
    ) -> Result<data::VerifiedClient, PluginFault> {
        payload_call(self, data::CLIENT_VERIFY, request).await
    }

    /// 读取所选 Key 对候选账号及上游模型的有效权限，不判断健康、额度或租约
    ///
    /// # Errors
    /// 身份不可用、查询无效或父调用取消时返回错误
    pub async fn check_access(
        &self,
        query: data::AccessQuery,
    ) -> Result<data::AccessFacts, PluginFault> {
        payload_call(self, data::ACCESS_CHECK, query).await
    }

    /// 读取 Key 当前启用状态与显式分组绑定，不返回密钥
    ///
    /// # Errors
    ///
    /// Key 不存在或宿主读取失败时返回错误
    pub async fn key_facts(
        &self,
        query: data::ClientKeyFactsQuery,
    ) -> Result<data::ClientKeyFacts, PluginFault> {
        payload_call(self, data::KEYS_GET, query).await
    }

    /// 通过宿主刷新账号额度观测，返回与 quota_facts 相同的非秘密投影
    /// 不修改上游额度，也不自动重置任何 Key
    ///
    /// # Errors
    /// 账号不支持刷新或 Provider 查询失败时返回错误
    pub async fn refresh_account_quota(
        &self,
        query: data::QuotaFactsQuery,
    ) -> Result<data::QuotaFacts, PluginFault> {
        payload_call(self, data::QUOTA_REFRESH, query).await
    }

    /// 分页读取账号基础事实
    ///
    /// # Errors
    ///
    /// 参数无效或宿主读取失败时返回错误
    pub async fn account_facts(
        &self,
        query: data::AccountFactsQuery,
    ) -> Result<data::AccountFactsPage, PluginFault> {
        payload_call(self, data::ACCOUNTS_LIST, query).await
    }

    /// 读取已有额度观测，不触发上游刷新，也不返回宿主预测结果
    ///
    /// # Errors
    ///
    /// 账号不存在或宿主读取失败时返回错误
    pub async fn quota_facts(
        &self,
        query: data::QuotaFactsQuery,
    ) -> Result<data::QuotaFacts, PluginFault> {
        payload_call(self, data::QUOTA_GET, query).await
    }

    /// 读取已有额度窗口标签、触顶与点数事实，不触发刷新或补算
    ///
    /// # Errors
    /// 账号不存在或宿主读取失败时返回错误
    pub async fn quota_details(
        &self,
        query: data::QuotaFactsQuery,
    ) -> Result<data::QuotaDetails, PluginFault> {
        payload_call(self, data::QUOTA_DETAILS, query).await
    }
}
