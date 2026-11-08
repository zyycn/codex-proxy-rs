//! 代理目录的安全投影与受管 HTTP 出口解析，配置事实由 Admin 持有

use std::sync::{Arc, OnceLock, Weak};

use gateway_admin::{
    model::{AdminError, PageSize, proxies::ProxyListQuery},
    ports::proxy::PluginProxyAccess,
};
use gateway_core::account::OutboundProxy;
use gateway_plugin_sdk::{PluginFault, call::host as wire};

use super::{
    admin::{encode, map_admin_error},
    invalid,
};
use crate::RpcReply;

pub(crate) struct PluginProxyPortSlot {
    access: OnceLock<Weak<dyn PluginProxyAccess>>,
}

impl PluginProxyPortSlot {
    pub(crate) const fn new() -> Self {
        Self {
            access: OnceLock::new(),
        }
    }

    pub(crate) fn bind(&self, access: &Arc<dyn PluginProxyAccess>) -> Result<(), AdminError> {
        self.access
            .set(Arc::downgrade(access))
            .map_err(|_| AdminError::conflict("插件代理端口已经绑定"))
    }

    fn access(&self) -> Result<Arc<dyn PluginProxyAccess>, PluginFault> {
        self.access
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| map_admin_error(AdminError::unavailable("插件代理服务暂不可用")))
    }

    pub(super) async fn resolve(&self, id: &str) -> Result<OutboundProxy, PluginFault> {
        self.access()?.resolve(id).await.map_err(map_admin_error)
    }

    pub(super) async fn list(
        &self,
        params: serde_json::Value,
        payload: &[u8],
    ) -> Result<RpcReply, PluginFault> {
        if params != serde_json::json!({}) {
            return Err(invalid());
        }
        let request: wire::ProxyListRequest =
            serde_json::from_slice(payload).map_err(|_| invalid())?;
        let page = self
            .access()?
            .list(ProxyListQuery {
                page: request.page,
                page_size: PageSize::new(request.page_size).map_err(|_| invalid())?,
                search: request.search,
            })
            .await
            .map_err(map_admin_error)?;
        encode(&wire::ProxyListResult {
            items: page
                .items
                .into_iter()
                .map(|proxy| wire::ProxySummary {
                    id: proxy.id,
                    name: proxy.name,
                    enabled: proxy.enabled,
                })
                .collect(),
            total: page.total,
            page: page.page,
            page_size: page.page_size,
        })
    }
}
