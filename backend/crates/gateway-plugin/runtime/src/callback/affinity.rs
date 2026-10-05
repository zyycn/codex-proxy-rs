//! Provider 会话亲和读取；只查询既有记录，不推导或写入亲和事实

use std::sync::{Arc, OnceLock, Weak};

use gateway_admin::model::AdminError;
use gateway_core::{
    engine::{ModelRequestId, nested::AffinityLookupPort},
    identity::ProviderKind,
    provider_ports::ProviderSessionAffinityKey,
};
use gateway_plugin_sdk::{
    CallContext, PluginFault,
    call::host::{AffinityLookupRequest, AffinityLookupResult},
};

use super::{denied, invalid};
use crate::RpcReply;

pub(crate) struct PluginAffinityPortSlot {
    port: OnceLock<Weak<dyn AffinityLookupPort>>,
}

impl PluginAffinityPortSlot {
    pub(crate) const fn new() -> Self {
        Self {
            port: OnceLock::new(),
        }
    }

    pub(crate) fn bind(&self, port: &Arc<dyn AffinityLookupPort>) -> Result<(), AdminError> {
        self.port
            .set(Arc::downgrade(port))
            .map_err(|_| AdminError::conflict("插件亲和查询端口已经绑定"))
    }

    fn upgrade(&self) -> Result<Arc<dyn AffinityLookupPort>, PluginFault> {
        self.port.get().and_then(Weak::upgrade).ok_or_else(denied)
    }
}

pub(super) struct PluginAffinity {
    slot: Arc<PluginAffinityPortSlot>,
}

impl PluginAffinity {
    pub(super) fn new(slot: Arc<PluginAffinityPortSlot>) -> Self {
        Self { slot }
    }

    pub(super) async fn call(
        &self,
        context: &CallContext,
        params: serde_json::Value,
        payload: &[u8],
    ) -> Result<RpcReply, PluginFault> {
        if !payload.is_empty() {
            return Err(invalid());
        }
        let request: AffinityLookupRequest =
            serde_json::from_value(params).map_err(|_| invalid())?;
        let parent_request_id = context
            .request_id
            .as_ref()
            .ok_or_else(denied)
            .and_then(|request_id| ModelRequestId::new(request_id.clone()).map_err(|_| denied()))?;
        let provider = ProviderKind::new(request.provider).map_err(|_| invalid())?;
        let result = self
            .slot
            .upgrade()?
            .lookup(gateway_core::engine::nested::AffinityLookupRequest {
                parent_request_id,
                provider,
                key: ProviderSessionAffinityKey::try_new(request.key).map_err(|_| invalid())?,
            })
            .await
            .map_err(super::error::gateway)?;
        Ok(RpcReply {
            result: serde_json::to_value(AffinityLookupResult {
                account_id: result.map(|result| result.account().as_str().to_owned()),
            })
            .map_err(|_| invalid())?,
            payload: Vec::new(),
        })
    }
}
