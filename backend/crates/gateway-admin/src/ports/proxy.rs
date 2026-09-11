use async_trait::async_trait;
use gateway_core::account::OutboundProxy;

use super::store::AdminStoreResult;
use crate::model::{
    MutationContext, Revision,
    proxies::{
        NewProxy, ProxyListQuery, ProxyMutation, ProxyPage, ProxyRecord, ProxyTestResult,
        UpdateProxy,
    },
};

#[async_trait]
pub trait ProxyStore: Send + Sync {
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

#[async_trait]
pub trait ProxyProbe: Send + Sync {
    async fn test(&self, proxy: &OutboundProxy) -> ProxyTestResult;
}
