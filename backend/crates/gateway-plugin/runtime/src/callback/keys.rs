//! Key 目录与预算回调；实例身份由宿主冻结，写入授权在存储事务复验

use std::sync::{Arc, OnceLock, Weak};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use gateway_admin::{
    model::{
        AdminError, PageSize,
        client_keys::{ClientKeyBudgetPeriod, ResetClientKeyBudget, UpdateClientKeyBudgetLimits},
        plugin_client_keys::{PluginClientKeyCursor, PluginClientKeyListQuery},
        plugin_resources::PluginResourceOwner,
        plugins::instances::PluginInstance,
    },
    ports::plugin_client_keys::PluginClientKeyAccess,
};
use gateway_core::policy::ClientApiKeyId;
use gateway_plugin_sdk::{
    CallContext, PluginFault,
    call::{
        host::{ClientKey, KeyListRequest, KeyListResult},
        key_budgets,
    },
};

use super::{
    admin::{encode, map_admin_error, mutation_context},
    denied, invalid,
};
use crate::RpcReply;

pub(super) struct PluginClientKeys {
    owner: PluginResourceOwner,
    ports: Arc<PluginClientKeyPortSlot>,
}

impl PluginClientKeys {
    pub(super) fn new(instance: &PluginInstance, ports: Arc<PluginClientKeyPortSlot>) -> Self {
        Self {
            owner: PluginResourceOwner {
                instance_id: instance.id.clone(),
                artifact_sha256: instance.artifact_sha256.clone(),
                revision: instance.revision,
            },
            ports,
        }
    }

    pub(super) async fn call(
        &self,
        context: &CallContext,
        method: &str,
        params: serde_json::Value,
        payload: &[u8],
    ) -> Result<RpcReply, PluginFault> {
        if method == "host.keys.list" {
            return self.ports.list(params, payload).await;
        }
        if params != serde_json::json!({}) {
            return Err(invalid());
        }
        let access = self.ports.upgrade()?;
        match method {
            key_budgets::GET => {
                let request: key_budgets::GetKeyBudgetRequest =
                    serde_json::from_slice(payload).map_err(|_| invalid())?;
                let id = ClientApiKeyId::new(request.client_key_id).map_err(|_| invalid())?;
                let budget = access.budget(&id).await.map_err(map_admin_error)?;
                encode(&key_budgets::KeyBudget {
                    client_key_id: id.as_str().to_owned(),
                    daily_limit_usd: budget.limits.daily_usd.canonical(),
                    weekly_limit_usd: budget.limits.weekly_usd.canonical(),
                    daily_used_usd: budget.daily_used_usd.canonical(),
                    weekly_used_usd: budget.weekly_used_usd.canonical(),
                    daily_resets_at_ms: budget
                        .daily_resets_at
                        .map(|time| chrono::DateTime::<chrono::Utc>::from(time).timestamp_millis()),
                    weekly_resets_at_ms: budget
                        .weekly_resets_at
                        .map(|time| chrono::DateTime::<chrono::Utc>::from(time).timestamp_millis()),
                })
            }
            key_budgets::UPDATE_LIMITS => {
                let request: key_budgets::UpdateKeyBudgetLimitsRequest =
                    serde_json::from_slice(payload).map_err(|_| invalid())?;
                let command = UpdateClientKeyBudgetLimits {
                    id: ClientApiKeyId::new(request.client_key_id).map_err(|_| invalid())?,
                    daily_limit_usd: request
                        .daily_limit_usd
                        .map(|value| value.parse())
                        .transpose()
                        .map_err(|_| invalid())?,
                    weekly_limit_usd: request
                        .weekly_limit_usd
                        .map(|value| value.parse())
                        .transpose()
                        .map_err(|_| invalid())?,
                };
                let id = access
                    .update_budget_limits(&self.owner, command, &mutation_context(context))
                    .await
                    .map_err(map_admin_error)?;
                encode(&key_budgets::UpdateKeyBudgetLimitsResult {
                    client_key_id: id.as_str().to_owned(),
                })
            }
            key_budgets::RESET => {
                let request: key_budgets::ResetKeyBudgetRequest =
                    serde_json::from_slice(payload).map_err(|_| invalid())?;
                let command = ResetClientKeyBudget {
                    id: ClientApiKeyId::new(request.client_key_id).map_err(|_| invalid())?,
                    period: match request.period {
                        key_budgets::BudgetPeriod::Daily => ClientKeyBudgetPeriod::Daily,
                        key_budgets::BudgetPeriod::Weekly => ClientKeyBudgetPeriod::Weekly,
                        key_budgets::BudgetPeriod::All => ClientKeyBudgetPeriod::All,
                    },
                };
                let id = access
                    .reset_budget(&self.owner, command, &mutation_context(context))
                    .await
                    .map_err(map_admin_error)?;
                encode(&key_budgets::ResetKeyBudgetResult {
                    client_key_id: id.as_str().to_owned(),
                })
            }
            _ => Err(denied()),
        }
    }
}

pub(crate) struct PluginClientKeyPortSlot {
    access: OnceLock<Weak<dyn PluginClientKeyAccess>>,
}

impl PluginClientKeyPortSlot {
    pub(crate) const fn new() -> Self {
        Self {
            access: OnceLock::new(),
        }
    }

    pub(crate) fn bind(&self, access: &Arc<dyn PluginClientKeyAccess>) -> Result<(), AdminError> {
        self.access
            .set(Arc::downgrade(access))
            .map_err(|_| AdminError::conflict("插件 Client Key 端口已经绑定"))
    }

    pub(super) fn upgrade(&self) -> Result<Arc<dyn PluginClientKeyAccess>, PluginFault> {
        self.access.get().and_then(Weak::upgrade).ok_or_else(denied)
    }

    pub(super) async fn list(
        &self,
        params: serde_json::Value,
        payload: &[u8],
    ) -> Result<RpcReply, PluginFault> {
        if !payload.is_empty() {
            return Err(invalid());
        }
        let request: KeyListRequest = serde_json::from_value(params).map_err(|_| invalid())?;
        let limit = PageSize::new(request.limit).map_err(|_| invalid())?;
        let cursor = request
            .cursor
            .map(|cursor| {
                if cursor.len() > 2048 {
                    return Err(invalid());
                }
                let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| invalid())?;
                let (name, id): (String, String) =
                    serde_json::from_slice(&bytes).map_err(|_| invalid())?;
                Ok(PluginClientKeyCursor {
                    name,
                    id: ClientApiKeyId::new(id).map_err(|_| invalid())?,
                })
            })
            .transpose()?;
        let access = self.upgrade()?;
        let page = access
            .list(PluginClientKeyListQuery { cursor, limit })
            .await
            .map_err(map_admin_error)?;
        let next_cursor = page
            .next_cursor
            .map(|cursor| {
                serde_json::to_vec(&(cursor.name, cursor.id.as_str()))
                    .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
                    .map_err(|_| invalid())
            })
            .transpose()?;
        Ok(RpcReply {
            result: serde_json::to_value(KeyListResult {
                keys: page
                    .items
                    .into_iter()
                    .map(|key| ClientKey {
                        id: key.id.as_str().to_owned(),
                        name: key.name,
                        enabled: key.enabled,
                    })
                    .collect(),
                next_cursor,
            })
            .map_err(|_| invalid())?,
            payload: Vec::new(),
        })
    }
}
