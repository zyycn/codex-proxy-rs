//! Outbound proxy pool management use cases.

use std::sync::Arc;

use async_trait::async_trait;
use gateway_core::runtime::SnapshotControl;
use uuid::Uuid;

use crate::{
    model::{
        AdminError, MutationContext,
        proxies::{
            CreateOutboundProxy, DeleteOutboundProxy, NewOutboundProxy, OutboundProxyId,
            OutboundProxyListQuery, OutboundProxyMutation, OutboundProxyPage,
            OutboundProxyTestReport, OutboundProxyTestSubject, RevealedOutboundProxy,
            TestOutboundProxy, UpdateOutboundProxy,
        },
    },
    ports::{proxy::OutboundProxyProbe, store::OutboundProxyStore},
};

use super::{map_store_error, publish_committed};

/// 未显式指定测试链接时的默认目标；任何 HTTP 响应都视为链路可达。
pub const DEFAULT_PROXY_TEST_TARGET: &str = "https://api.openai.com/v1/models";

/// API-facing outbound proxy pool service.
#[async_trait]
pub trait OutboundProxyService: Send + Sync {
    async fn list(&self, query: OutboundProxyListQuery) -> Result<OutboundProxyPage, AdminError>;
    async fn create(
        &self,
        context: &MutationContext,
        command: CreateOutboundProxy,
    ) -> Result<OutboundProxyMutation, AdminError>;
    async fn update(
        &self,
        context: &MutationContext,
        command: UpdateOutboundProxy,
    ) -> Result<OutboundProxyMutation, AdminError>;
    async fn delete(
        &self,
        context: &MutationContext,
        command: DeleteOutboundProxy,
    ) -> Result<OutboundProxyMutation, AdminError>;
    async fn reveal(&self, id: &OutboundProxyId) -> Result<RevealedOutboundProxy, AdminError>;
    async fn test(&self, command: TestOutboundProxy)
    -> Result<OutboundProxyTestReport, AdminError>;
}

pub(crate) struct DefaultOutboundProxyService {
    store: Arc<dyn OutboundProxyStore>,
    probe: Arc<dyn OutboundProxyProbe>,
    snapshot: Arc<dyn SnapshotControl>,
}

impl DefaultOutboundProxyService {
    #[must_use]
    pub(crate) fn new(
        store: Arc<dyn OutboundProxyStore>,
        probe: Arc<dyn OutboundProxyProbe>,
        snapshot: Arc<dyn SnapshotControl>,
    ) -> Self {
        Self {
            store,
            probe,
            snapshot,
        }
    }

    async fn publish(
        &self,
        result: Result<OutboundProxyMutation, crate::ports::store::AdminStoreError>,
    ) -> Result<OutboundProxyMutation, AdminError> {
        let result = result.map_err(|error| map_store_error(error, "outbound proxy"))?;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }
}

#[async_trait]
impl OutboundProxyService for DefaultOutboundProxyService {
    async fn list(&self, query: OutboundProxyListQuery) -> Result<OutboundProxyPage, AdminError> {
        self.store
            .list_outbound_proxies(query)
            .await
            .map_err(|error| map_store_error(error, "outbound proxy"))
    }

    async fn create(
        &self,
        context: &MutationContext,
        command: CreateOutboundProxy,
    ) -> Result<OutboundProxyMutation, AdminError> {
        let id = OutboundProxyId::new(format!("pxy_{}", Uuid::now_v7().simple()))
            .map_err(|_| AdminError::internal("创建出站代理 ID 失败"))?;
        self.publish(
            self.store
                .create_outbound_proxy(
                    NewOutboundProxy {
                        id,
                        name: command.name,
                        proxy: command.proxy,
                    },
                    context,
                )
                .await,
        )
        .await
    }

    async fn update(
        &self,
        context: &MutationContext,
        command: UpdateOutboundProxy,
    ) -> Result<OutboundProxyMutation, AdminError> {
        self.publish(self.store.update_outbound_proxy(command, context).await)
            .await
    }

    async fn delete(
        &self,
        context: &MutationContext,
        command: DeleteOutboundProxy,
    ) -> Result<OutboundProxyMutation, AdminError> {
        self.publish(self.store.delete_outbound_proxy(command, context).await)
            .await
    }

    async fn reveal(&self, id: &OutboundProxyId) -> Result<RevealedOutboundProxy, AdminError> {
        self.store
            .load_outbound_proxy(id)
            .await
            .map_err(|error| map_store_error(error, "outbound proxy"))?
            .ok_or_else(|| AdminError::not_found("出站代理不存在"))
    }

    async fn test(
        &self,
        command: TestOutboundProxy,
    ) -> Result<OutboundProxyTestReport, AdminError> {
        let proxy = match command.subject {
            OutboundProxyTestSubject::Saved(id) => self.reveal(&id).await?.proxy,
            OutboundProxyTestSubject::Inline(proxy) => proxy,
        };
        let target_url = command
            .target_url
            .unwrap_or_else(|| DEFAULT_PROXY_TEST_TARGET.to_owned());
        Ok(self.probe.probe(&proxy, &target_url).await)
    }
}
