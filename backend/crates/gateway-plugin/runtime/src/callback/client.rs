//! 客户端认证、冻结权限查询与调用内身份复用

use std::sync::{Arc, OnceLock, Weak};

use gateway_admin::model::AdminError;
use gateway_core::{
    account::ProviderAccountId,
    engine::{
        authentication::{ClientAuthenticationRequest, ClientIdentityPort},
        execution::AuthenticatedClient,
    },
    policy::ClientApiKeyId,
    routing::UpstreamModelId,
};
use gateway_plugin_sdk::{CallContext, ErrorCode, PluginFault, call::data};

use super::{CallResources, denied, invalid};
use crate::RpcReply;

const MAX_CLIENT_IDENTITIES_PER_CALL: usize = 16;

pub(crate) struct PluginClientIdentityPortSlot {
    port: OnceLock<Weak<dyn ClientIdentityPort>>,
}

impl PluginClientIdentityPortSlot {
    pub(crate) const fn new() -> Self {
        Self {
            port: OnceLock::new(),
        }
    }

    pub(crate) fn bind(&self, port: &Arc<dyn ClientIdentityPort>) -> Result<(), AdminError> {
        self.port
            .set(Arc::downgrade(port))
            .map_err(|_| AdminError::conflict("插件身份端口已经绑定"))
    }

    fn upgrade(&self) -> Result<Arc<dyn ClientIdentityPort>, PluginFault> {
        self.port.get().and_then(Weak::upgrade).ok_or_else(denied)
    }
}

pub(super) struct PluginClients {
    slot: Arc<PluginClientIdentityPortSlot>,
}

impl PluginClients {
    pub(super) fn new(slot: Arc<PluginClientIdentityPortSlot>) -> Self {
        Self { slot }
    }

    pub(super) async fn call(
        &self,
        context: &CallContext,
        call: &Arc<CallResources>,
        method: &str,
        params: serde_json::Value,
        payload: Vec<u8>,
    ) -> Result<RpcReply, PluginFault> {
        match method {
            data::CLIENT_VERIFY => {
                if params != serde_json::json!({}) {
                    return Err(invalid());
                }
                let request: data::VerifyClientRequest =
                    serde_json::from_slice(&payload).map_err(|_| invalid())?;
                let mut authentication = ClientAuthenticationRequest::new(request.authorization)
                    .map_err(|_| invalid())?
                    .with_invocation(call.child_origin(context)?);
                if let Some(settings) = call.request_settings() {
                    authentication = authentication.with_settings(settings);
                }
                let port = self.slot.upgrade()?;
                call.observe_dispatch()?;
                let client = port
                    .verify_client(authentication)
                    .await
                    .map_err(super::error::gateway)?;
                super::admin::encode(&data::VerifiedClient {
                    client_key_id: client.policy().key_id().as_str().to_owned(),
                })
            }
            data::ACCESS_CHECK => {
                if params != serde_json::json!({}) {
                    return Err(invalid());
                }
                let request: data::AccessQuery =
                    serde_json::from_slice(&payload).map_err(|_| invalid())?;
                let client = self.identity(call, request.client_key_id).await?;
                let scope = client.policy().account_scope();
                let model = request
                    .upstream_model
                    .map(UpstreamModelId::new)
                    .transpose()
                    .map_err(|_| invalid())?;
                let accounts = request
                    .account_ids
                    .into_iter()
                    .map(|account_id| {
                        let account =
                            ProviderAccountId::new(account_id.clone()).map_err(|_| invalid())?;
                        let allowed = model.as_ref().map_or_else(
                            || scope.allows(&account),
                            |model| scope.allows_model(&account, model.as_str()),
                        );
                        Ok(data::AccountAccessFacts {
                            account_id,
                            allowed,
                        })
                    })
                    .collect::<Result<Vec<_>, PluginFault>>()?;
                super::admin::encode(&data::AccessFacts {
                    schema_version: 1,
                    client_key_id: client.policy().key_id().as_str().to_owned(),
                    accounts,
                })
            }
            _ => Err(denied()),
        }
    }

    pub(super) async fn identity(
        &self,
        call: &CallResources,
        client_key_id: String,
    ) -> Result<AuthenticatedClient, PluginFault> {
        let port = self.slot.upgrade()?;
        let key = ClientApiKeyId::new(client_key_id.clone()).map_err(|_| invalid())?;
        let settings = call.request_settings();
        let mut identities = call.client_identities.lock().await;
        if let Some(client) = identities
            .get(&client_key_id)
            .filter(|client| client.request_settings() == settings.as_ref())
        {
            return Ok(client.clone());
        }
        if !identities.contains_key(&client_key_id)
            && identities.len() >= MAX_CLIENT_IDENTITIES_PER_CALL
        {
            return Err(PluginFault::new(
                ErrorCode::Capacity,
                "client identity capacity is exhausted",
            ));
        }
        let client = port
            .resolve_client(key, settings)
            .await
            .map_err(super::error::gateway)?;
        // 查询与执行共用调用内快照，执行链和子执行取消只在真正启动时建立
        identities.insert(client_key_id, client.clone());
        Ok(client)
    }
}
