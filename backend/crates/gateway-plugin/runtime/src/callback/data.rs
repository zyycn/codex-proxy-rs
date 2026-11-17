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
            data::QUOTA_HISTORY => return self.quota_history(payload).await,
            data::QUOTA_GET | data::QUOTA_REFRESH | data::QUOTA_DETAILS => {
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
                if method == data::QUOTA_DETAILS {
                    return super::admin::encode(&data::QuotaDetails {
                        schema_version: 1,
                        account_id: account_id.as_str().to_owned(),
                        observed_at_ms: quota.observed_at.map(|time| time.timestamp_millis()),
                        limit_reached: quota.limit_reached,
                        windows: quota
                            .windows
                            .into_iter()
                            .map(|window| data::QuotaWindowDetails {
                                key: window.key,
                                group: window.group,
                                label: window.label,
                                limit_id: window.limit_id,
                                limit_name: window.limit_name,
                                role: window.role.map(|role| role.as_str().to_owned()),
                                window_seconds: window.window_seconds,
                                used_percent: window.used_percent,
                                reset_at_ms: window.reset_at.map(|time| time.timestamp_millis()),
                                limit_reached: window.limit_reached,
                            })
                            .collect(),
                        credits: quota.credits.map(|credits| data::QuotaCredits {
                            has_credits: credits.has_credits,
                            unlimited: credits.unlimited,
                            balance: credits.balance,
                        }),
                    });
                }
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

    async fn quota_history(&self, payload: &[u8]) -> Result<RpcReply, PluginFault> {
        use gateway_admin::model::{
            observability::TimeRange,
            quota_history::{QuotaHistoryPoint, QuotaHistoryQuery},
        };

        let wire: data::QuotaHistoryQuery =
            serde_json::from_slice(payload).map_err(|_| invalid())?;
        let id = ProviderAccountId::new(wire.account_id).map_err(|_| invalid())?;
        let time = |ms| chrono::DateTime::from_timestamp_millis(ms).ok_or_else(invalid);
        let range =
            TimeRange::new(time(wire.start_ms)?, time(wire.end_ms)?).map_err(|_| invalid())?;
        if wire.start_ms < 0
            || wire.window_key.is_empty()
            || wire.window_key.len() > 256
            || wire.window_key.chars().any(char::is_control)
        {
            return Err(invalid());
        }
        let query = QuotaHistoryQuery {
            window_key: wire.window_key,
            range,
            cursor: wire
                .cursor
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .map_err(|_| invalid())?,
        };
        if query.cursor.as_ref().is_some_and(|c| {
            c.completed_at >= range.end
                || c.completed_at < query.scan_start()
                || c.id.is_empty()
                || c.id.contains('\0')
        }) {
            return Err(invalid());
        }
        let page = self
            .accounts
            .upgrade()
            .map_err(map_admin_error)?
            .quota_history(&id, query)
            .await
            .map_err(map_admin_error)?;
        let sample = |p: QuotaHistoryPoint| data::QuotaHistorySample {
            observed_at_ms: p.observed_at.timestamp_millis(),
            used_percent: p.reading.used_percent,
            reset_at_ms: p.reading.reset_at.map(|at| at.timestamp_millis()),
            plan_type: p.reading.plan_type,
        };
        super::admin::encode(&data::QuotaHistoryPage {
            schema_version: 1,
            samples: page.samples.into_iter().map(sample).collect(),
            baseline: page.baseline.map(sample),
            next_cursor: page
                .next_cursor
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|_| invalid())?,
        })
    }
}
