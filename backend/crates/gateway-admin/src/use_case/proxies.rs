use std::sync::Arc;

use async_trait::async_trait;
use gateway_core::runtime::SnapshotControl;
use tokio::sync::Semaphore;

use super::{map_store_error, publish_committed};
use crate::{
    model::{AdminError, AdminErrorKind, MutationContext, Revision, proxies::*},
    ports::proxy::{ProxyProbe, ProxyStore},
};

#[async_trait]
pub trait ProxiesService: Send + Sync {
    async fn list(&self, query: ProxyListQuery) -> Result<ProxyPage, AdminError>;
    async fn create(
        &self,
        command: NewProxy,
        context: &MutationContext,
    ) -> Result<ProxyMutation, AdminError>;
    async fn update(
        &self,
        command: UpdateProxy,
        context: &MutationContext,
    ) -> Result<ProxyMutation, AdminError>;
    async fn delete(
        &self,
        id: &str,
        revision: Revision,
        context: &MutationContext,
    ) -> Result<Revision, AdminError>;
    async fn test(
        &self,
        id: &str,
        revision: Revision,
        context: &MutationContext,
    ) -> Result<ProxyRecord, AdminError>;
}

pub(crate) struct DefaultProxiesService {
    store: Arc<dyn ProxyStore>,
    probe: Arc<dyn ProxyProbe>,
    snapshot: Arc<dyn SnapshotControl>,
    test_slots: Semaphore,
}

impl DefaultProxiesService {
    pub(crate) fn new(
        store: Arc<dyn ProxyStore>,
        probe: Arc<dyn ProxyProbe>,
        snapshot: Arc<dyn SnapshotControl>,
    ) -> Self {
        Self {
            store,
            probe,
            snapshot,
            test_slots: Semaphore::new(4),
        }
    }
}

#[async_trait]
impl ProxiesService for DefaultProxiesService {
    async fn list(&self, query: ProxyListQuery) -> Result<ProxyPage, AdminError> {
        if query.page == 0 || query.search.len() > 256 || query.search.chars().any(char::is_control)
        {
            return Err(AdminError::invalid("代理查询参数不合法"));
        }
        self.store
            .list(query)
            .await
            .map_err(|error| map_store_error(error, "proxy"))
    }

    async fn create(
        &self,
        mut command: NewProxy,
        context: &MutationContext,
    ) -> Result<ProxyMutation, AdminError> {
        command.name = validate_name(&command.name)?;
        let result = self
            .store
            .create(command, context)
            .await
            .map_err(|error| map_store_error(error, "proxy"))?;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }

    async fn update(
        &self,
        mut command: UpdateProxy,
        context: &MutationContext,
    ) -> Result<ProxyMutation, AdminError> {
        command.name = validate_name(&command.name)?;
        let result = self
            .store
            .update(command, context)
            .await
            .map_err(|error| map_store_error(error, "proxy"))?;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }

    async fn delete(
        &self,
        id: &str,
        revision: Revision,
        context: &MutationContext,
    ) -> Result<Revision, AdminError> {
        let result = self
            .store
            .delete(id, revision, context)
            .await
            .map_err(|error| map_store_error(error, "proxy"))?;
        publish_committed(self.snapshot.as_ref(), result).await?;
        Ok(result)
    }

    async fn test(
        &self,
        id: &str,
        revision: Revision,
        context: &MutationContext,
    ) -> Result<ProxyRecord, AdminError> {
        let _permit = self.test_slots.try_acquire().map_err(|_| {
            AdminError::new(AdminErrorKind::RateLimited, "代理测试繁忙，请稍后重试")
        })?;
        let record = self
            .store
            .get(id)
            .await
            .map_err(|error| map_store_error(error, "proxy"))?;
        if record.revision != revision {
            return Err(AdminError::conflict("代理已被修改，请刷新后重新测试"));
        }
        let result = self.probe.test(&record.proxy).await;
        self.store
            .record_test(id, revision, result, context)
            .await
            .map_err(|error| map_store_error(error, "proxy"))
    }
}

fn validate_name(value: &str) -> Result<String, AdminError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 100 || value.chars().any(char::is_control) {
        return Err(AdminError::invalid("代理名称需要 1 至 100 个字符"));
    }
    Ok(value.to_owned())
}
