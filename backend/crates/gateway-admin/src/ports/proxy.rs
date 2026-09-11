use async_trait::async_trait;
use gateway_core::account::OutboundProxy;

use super::store::AdminStoreResult;
use crate::model::{
    MutationContext, Revision,
    proxies::{
        ImportProxyBinding, NewProxy, ProxyListQuery, ProxyMutation, ProxyPage, ProxyRecord,
        ProxyTestResult, UpdateProxy,
    },
};

#[async_trait]
pub trait ProxyStore: Send + Sync {
    /// 在凭据交换到提交期间保护选定代理的连接配置和测试结果。
    async fn reserve_import(&self, id: &str) -> AdminStoreResult<ProxyImportReservation>;
    async fn list(&self, query: ProxyListQuery) -> AdminStoreResult<ProxyPage>;
    async fn get(&self, id: &str) -> AdminStoreResult<ProxyRecord>;
    async fn create(
        &self,
        command: NewProxy,
        context: &MutationContext,
    ) -> AdminStoreResult<ProxyMutation>;
    async fn update(
        &self,
        command: UpdateProxy,
        context: &MutationContext,
    ) -> AdminStoreResult<ProxyMutation>;
    async fn delete(
        &self,
        id: &str,
        revision: Revision,
        context: &MutationContext,
    ) -> AdminStoreResult<Revision>;
    async fn record_test(
        &self,
        id: &str,
        revision: Revision,
        result: ProxyTestResult,
        context: &MutationContext,
    ) -> AdminStoreResult<ProxyRecord>;
}

/// 离开作用域时释放保护，错误返回和请求取消也遵循相同规则。
pub trait ProxyImportGuard: Send + Sync {}

pub struct ProxyImportReservation {
    pub binding: ImportProxyBinding,
    pub guard: Box<dyn ProxyImportGuard>,
}

#[async_trait]
pub trait ProxyProbe: Send + Sync {
    async fn test(&self, proxy: &OutboundProxy) -> ProxyTestResult;
}
