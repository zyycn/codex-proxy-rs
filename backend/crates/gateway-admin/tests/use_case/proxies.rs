use async_trait::async_trait;
use gateway_admin::{
    model::{MutationContext, Revision, proxies::*},
    ports::{
        proxy::{ProxyProbe, ProxyStore},
        store::AdminStoreResult,
    },
};
use gateway_core::account::OutboundProxy;

pub(super) struct UnavailableProxies;

#[async_trait]
impl ProxyStore for UnavailableProxies {
    async fn list(&self, _: ProxyListQuery) -> AdminStoreResult<ProxyPage> {
        Err(super::unavailable("proxy"))
    }
    async fn get(&self, _: &str) -> AdminStoreResult<ProxyRecord> {
        Err(super::unavailable("proxy"))
    }
    async fn create(&self, _: NewProxy, _: &MutationContext) -> AdminStoreResult<ProxyMutation> {
        Err(super::unavailable("proxy"))
    }
    async fn update(&self, _: UpdateProxy, _: &MutationContext) -> AdminStoreResult<ProxyMutation> {
        Err(super::unavailable("proxy"))
    }
    async fn delete(
        &self,
        _: &str,
        _: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(super::unavailable("proxy"))
    }
    async fn record_test(
        &self,
        _: &str,
        _: Revision,
        _: ProxyTestResult,
        _: &MutationContext,
    ) -> AdminStoreResult<ProxyRecord> {
        Err(super::unavailable("proxy"))
    }
}

#[async_trait]
impl ProxyProbe for UnavailableProxies {
    async fn test(&self, _: &OutboundProxy) -> ProxyTestResult {
        panic!("unexpected proxy probe")
    }
}
