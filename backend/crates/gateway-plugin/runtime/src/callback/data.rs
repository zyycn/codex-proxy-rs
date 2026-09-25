use std::sync::Arc;

use gateway_admin::model::{
    PageSize, plugins::instances::PluginPermissionGrant,
    provider_credentials::PluginAccountListQuery,
};
use gateway_core::{account::ProviderAccountId, routing::ProviderKind};
use gateway_plugin_sdk::{CallContext, PluginFault, Stage, call::data};

use super::{
    accounts::{PluginAccountPortSlot, map_admin_error},
    denied, invalid,
};
use crate::RpcReply;

pub(super) struct PluginData {
    accounts: Arc<PluginAccountPortSlot>,
    authorized: bool,
}

impl PluginData {
    pub(super) fn new(
        accounts: Arc<PluginAccountPortSlot>,
        grants: &[PluginPermissionGrant],
    ) -> Self {
        Self {
            accounts,
            authorized: grants.iter().any(|grant| grant.permission == "data"),
        }
    }

    pub(super) async fn call(
        &self,
        context: &CallContext,
        method: &str,
        params: serde_json::Value,
        payload: &[u8],
    ) -> Result<RpcReply, PluginFault> {
        // 管理范围的只读授权不继承到客户端请求链，避免借数据查询扩大当前 Key 的范围。
        if !self.authorized || !matches!(context.stage, Stage::Management | Stage::CommandLine) {
            return Err(denied());
        }
        if params != serde_json::json!({}) {
            return Err(invalid());
        }
        let accounts = self.accounts.upgrade().map_err(map_admin_error)?;
        let payload = match method {
            data::ACCOUNTS_LIST => {
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
            data::QUOTA_GET => {
                let query: data::QuotaFactsQuery =
                    serde_json::from_slice(payload).map_err(|_| invalid())?;
                let account_id = ProviderAccountId::new(query.account_id).map_err(|_| invalid())?;
                let quota = accounts
                    .get_quota(&account_id)
                    .await
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
