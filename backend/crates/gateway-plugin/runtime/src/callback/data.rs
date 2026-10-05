//! 插件非秘密账号、Key 与额度事实查询的宿主回调

use std::sync::Arc;

use gateway_admin::model::{PageSize, provider_credentials::PluginAccountListQuery};
use gateway_core::{account::ProviderAccountId, policy::ClientApiKeyId, routing::ProviderKind};
use gateway_plugin_sdk::{PluginFault, call::data};

use super::{
    accounts::PluginAccountPortSlot, admin::map_admin_error, denied, invalid,
    keys::PluginClientKeyPortSlot,
};
use crate::RpcReply;

pub(super) struct PluginData {
    accounts: Arc<PluginAccountPortSlot>,
    keys: Arc<PluginClientKeyPortSlot>,
}

impl PluginData {
    pub(super) fn new(
        accounts: Arc<PluginAccountPortSlot>,
        keys: Arc<PluginClientKeyPortSlot>,
    ) -> Self {
        Self { accounts, keys }
    }

    pub(super) async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
        payload: &[u8],
    ) -> Result<RpcReply, PluginFault> {
        if params != serde_json::json!({}) {
            return Err(invalid());
        }
        let payload = match method {
            data::KEYS_GET => {
                let query: data::ClientKeyFactsQuery =
                    serde_json::from_slice(payload).map_err(|_| invalid())?;
                let id = ClientApiKeyId::new(query.client_key_id).map_err(|_| invalid())?;
                let key = self
                    .keys
                    .upgrade()?
                    .facts(&id)
                    .await
                    .map_err(map_admin_error)?;
                serde_json::to_vec(&data::ClientKeyFacts {
                    schema_version: 1,
                    client_key_id: key.id.as_str().to_owned(),
                    enabled: key.enabled,
                    group_ids: key
                        .group_ids
                        .into_iter()
                        .map(|id| id.as_str().to_owned())
                        .collect(),
                })
            }
            data::ACCOUNTS_LIST => {
                let accounts = self.accounts.upgrade().map_err(map_admin_error)?;
                let query: data::AccountFactsQuery =
                    serde_json::from_slice(payload).map_err(|_| invalid())?;
                let page = accounts
                    .list(PluginAccountListQuery {
                        provider_kind: query
                            .provider_id
                            .map(ProviderKind::new)
                            .transpose()
                            .map_err(|_| invalid())?,
                        cursor: query
                            .cursor
                            .map(ProviderAccountId::new)
                            .transpose()
                            .map_err(|_| invalid())?,
                        limit: PageSize::new(query.limit).map_err(|_| invalid())?,
                    })
                    .await
                    .map_err(map_admin_error)?;
                serde_json::to_vec(&data::AccountFactsPage {
                    schema_version: 1,
                    accounts: page
                        .accounts
                        .into_iter()
                        .map(|account| data::AccountFacts {
                            account_id: account.id,
                            provider_id: account.provider_kind.as_str().to_owned(),
                            name: account.name,
                            email: account.email,
                            group_ids: account
                                .groups
                                .into_iter()
                                .map(|group| group.id.as_str().to_owned())
                                .collect(),
                            enabled: account.enabled,
                            updated_at_ms: account.updated_at.timestamp_millis(),
                        })
                        .collect(),
                    next_cursor: page.next_cursor.map(|id| id.as_str().to_owned()),
                })
            }
            data::QUOTA_GET | data::QUOTA_REFRESH => {
                let accounts = self.accounts.upgrade().map_err(map_admin_error)?;
                let query: data::QuotaFactsQuery =
                    serde_json::from_slice(payload).map_err(|_| invalid())?;
                let account_id = ProviderAccountId::new(query.account_id).map_err(|_| invalid())?;
                let quota = if method == data::QUOTA_REFRESH {
                    accounts.refresh_quota(&account_id).await
                } else {
                    accounts.get_quota(&account_id).await
                }
                .map_err(map_admin_error)?;
                serde_json::to_vec(&data::QuotaFacts {
                    schema_version: 1,
                    account_id: account_id.as_str().to_owned(),
                    observed_at_ms: quota.observed_at.map(|time| time.timestamp_millis()),
                    windows: quota
                        .windows
                        .into_iter()
                        .map(|window| data::QuotaWindowFacts {
                            key: window.key,
                            window_seconds: window.window_seconds,
                            used_percent: window.used_percent,
                            reset_at_ms: window.reset_at.map(|time| time.timestamp_millis()),
                        })
                        .collect(),
                })
            }
            _ => return Err(denied()),
        }
        .map_err(|_| invalid())?;
        Ok(RpcReply {
            result: serde_json::json!({}),
            payload,
        })
    }
}
