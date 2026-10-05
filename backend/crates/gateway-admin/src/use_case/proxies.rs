//! 账号出站代理的管理、账号解绑与连通性测试用例

use std::sync::Arc;

use async_trait::async_trait;
use futures::{StreamExt, stream};
use gateway_core::{
    account::{OutboundProxy, ProviderAccountId},
    routing::ProviderKind,
    runtime::SnapshotControl,
};
use tokio::sync::Semaphore;

use super::{map_store_error, publish_committed};
use crate::{
    model::{
        AdminError, AdminErrorKind, MutationContext, Revision,
        provider_credentials::{ProviderQuotaRequest, explicit_plan_type},
        proxies::*,
    },
    ports::{
        provider::ProviderAdminRegistry,
        proxy::{ProxyProbe, ProxyStore},
        store::AdminStoreErrorKind,
    },
};

#[async_trait]
pub trait ProxiesService: Send + Sync {
    async fn list(&self, query: ProxyListQuery) -> Result<ProxyPage, AdminError>;
    async fn list_accounts(
        &self,
        query: ProxyAccountListQuery,
    ) -> Result<ProxyAccountPage, AdminError>;
    async fn remove_account(
        &self,
        proxy_id: &str,
        account_id: &str,
        context: &MutationContext,
    ) -> Result<Revision, AdminError>;
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
        detect_location: bool,
        context: &MutationContext,
    ) -> Result<ProxyRecord, AdminError>;
    /// 探测未保存的连接地址，不写入代理记录或修改账号绑定
    async fn probe(
        &self,
        proxy: &OutboundProxy,
        detect_location: bool,
    ) -> Result<ProxyTestResult, AdminError>;
}

pub(crate) struct DefaultProxiesService {
    store: Arc<dyn ProxyStore>,
    probe: Arc<dyn ProxyProbe>,
    snapshot: Arc<dyn SnapshotControl>,
    providers: ProviderAdminRegistry,
    test_slots: Semaphore,
}

impl DefaultProxiesService {
    pub(crate) fn new(
        store: Arc<dyn ProxyStore>,
        probe: Arc<dyn ProxyProbe>,
        snapshot: Arc<dyn SnapshotControl>,
        providers: ProviderAdminRegistry,
    ) -> Self {
        Self {
            store,
            probe,
            snapshot,
            providers,
            test_slots: Semaphore::new(4),
        }
    }
}

#[async_trait]
impl ProxiesService for DefaultProxiesService {
    async fn remove_account(
        &self,
        proxy_id: &str,
        account_id: &str,
        context: &MutationContext,
    ) -> Result<Revision, AdminError> {
        if proxy_id.is_empty() || proxy_id.len() > 128 || proxy_id.chars().any(char::is_control) {
            return Err(AdminError::invalid("代理 ID 不合法"));
        }
        let account_id = ProviderAccountId::new(account_id.to_owned())
            .map_err(|_| AdminError::invalid("账号 ID 不合法"))?;
        let revision = self
            .store
            .remove_account(proxy_id, &account_id, context)
            .await
            .map_err(|error| {
                if error.kind() == AdminStoreErrorKind::Conflict {
                    AdminError::conflict("账号的代理绑定已变化，请刷新后重试")
                } else {
                    map_store_error(error, "proxy account")
                }
            })?;
        publish_committed(self.snapshot.as_ref(), revision).await?;
        Ok(revision)
    }

    async fn list_accounts(
        &self,
        query: ProxyAccountListQuery,
    ) -> Result<ProxyAccountPage, AdminError> {
        if query.proxy_id.is_empty()
            || query.proxy_id.len() > 128
            || query.proxy_id.chars().any(char::is_control)
            || query.page == 0
            || query.search.len() > 256
            || query.search.chars().any(char::is_control)
        {
            return Err(AdminError::invalid("关联账号查询参数不合法"));
        }
        let mut page = self
            .store
            .list_accounts(query)
            .await
            .map_err(|error| map_store_error(error, "proxy"))?;
        page.items = stream::iter(page.items)
            .map(|mut account| async {
                // 与账号目录一致，缺失套餐时读取已有额度快照；限制并发且不触发上游刷新
                let mut cached_quota = None;
                if explicit_plan_type(account.plan_type.as_deref()).is_none()
                    && let Ok(kind) = ProviderKind::new(account.provider_kind.clone())
                    && let Ok(provider) = self.providers.require(&kind)
                    && let Ok(account_id) = ProviderAccountId::new(account.id.clone())
                    && let Ok(quota) = provider
                        .quota(ProviderQuotaRequest {
                            account_id,
                            refresh: false,
                            rolling_usage: None,
                        })
                        .await
                {
                    cached_quota = Some(quota);
                }
                account.plan_type_display = self.providers.resolve_account_plan(
                    &account.provider_kind,
                    &mut account.plan_type,
                    cached_quota.as_ref(),
                );
                account
            })
            .buffered(8)
            .collect()
            .await;
        Ok(page)
    }

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
        command.location = command
            .location
            .map(|location| location.normalized())
            .transpose()
            .map_err(|_| AdminError::invalid("代理位置不合法"))?;
        command.test = if command.auto_location {
            Some(self.probe(&command.proxy, true).await?)
        } else {
            None
        };
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
        command.location = command
            .location
            .map(|location| location.map(|value| value.normalized()).transpose())
            .transpose()
            .map_err(|_| AdminError::invalid("代理位置不合法"))?;
        // 只在开启自动模式或连接地址变化时检测，普通编辑不刷新已识别位置
        command.test = None;
        if command.auto_location.is_some() || command.proxy.is_some() {
            let current = self
                .store
                .get(&command.id)
                .await
                .map_err(|error| map_store_error(error, "proxy"))?;
            if current.revision != command.revision {
                return Err(AdminError::conflict("代理已被修改，请刷新后重试"));
            }
            let proxy = command.proxy.as_ref().unwrap_or(&current.proxy);
            if command.auto_location.unwrap_or(current.auto_location)
                && (!current.auto_location || proxy != &current.proxy)
            {
                command.test = Some(self.probe(proxy, true).await?);
            }
        }
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

    async fn probe(
        &self,
        proxy: &OutboundProxy,
        detect_location: bool,
    ) -> Result<ProxyTestResult, AdminError> {
        let _permit = self.test_slots.try_acquire().map_err(|_| {
            AdminError::new(AdminErrorKind::RateLimited, "代理测试繁忙，请稍后重试")
        })?;
        Ok(self.probe.test(proxy, detect_location).await)
    }

    async fn test(
        &self,
        id: &str,
        revision: Revision,
        detect_location: bool,
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
        // 手动解析只为本次测试请求位置，不改变自动跟随的持久配置
        let result = self
            .probe
            .test(&record.proxy, record.auto_location || detect_location)
            .await;
        let mutation = self
            .store
            .record_test(id, revision, result, context)
            .await
            .map_err(|error| map_store_error(error, "proxy"))?;
        if record.effective_location() != mutation.record.effective_location() {
            publish_committed(self.snapshot.as_ref(), mutation.config_revision).await?;
        }
        Ok(mutation.record)
    }
}

fn validate_name(value: &str) -> Result<String, AdminError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 100 || value.chars().any(char::is_control) {
        return Err(AdminError::invalid("代理名称需要 1 至 100 个字符"));
    }
    Ok(value.to_owned())
}
