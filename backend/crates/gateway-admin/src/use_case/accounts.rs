//! 统一账号目录与跨 Provider 动态分派。

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use chrono::{Duration, Utc};
use futures::StreamExt as _;
use gateway_core::{
    engine::{
        credential::ProviderAccountId,
        probe::{AccountProbe, AccountProbeRequest},
    },
    routing::{ProviderKind, UpstreamModelId, snapshot::SnapshotControl},
};

use crate::{
    model::{
        AdminError, MutationContext,
        accounts::{
            AccountConnectionTestEvent, AccountConnectionTestEventStream, AccountListQuery,
            AccountPageItem, AccountUpdateResult, AccountUsageWindowQuery, AccountsUpdateResult,
            BatchUpdateAccounts, UpdateAccount,
        },
        observability::TimeRange,
        provider_credentials::{
            AccountDirectoryItem, AccountDirectoryPage, AccountExportBundle, AccountRefreshResult,
            PrepareCredentialRefresh, ProviderModels, ProviderQuota, ProviderQuotaRequest,
            ProviderQuotaWindow, QuotaLocalUsageAttribution,
        },
    },
    ports::{provider::ProviderAdminRegistry, store::AccountStore},
};

use super::{
    commit_credential_refresh, map_provider_error, map_store_error, publish_committed,
    validate_prepared_rotation,
};

const CONNECTION_TEST_INPUT: &str = "Reply with exactly OK.";

/// 统一账号页消费的服务。
#[async_trait]
pub trait AccountsService: Send + Sync {
    async fn list(&self, query: AccountListQuery) -> Result<AccountDirectoryPage, AdminError>;

    async fn export(
        &self,
        context: &MutationContext,
        account_ids: Vec<ProviderAccountId>,
    ) -> Result<AccountExportBundle, AdminError>;

    async fn refresh(
        &self,
        context: &MutationContext,
        account_id: ProviderAccountId,
    ) -> Result<AccountRefreshResult, AdminError>;

    async fn recover(
        &self,
        context: &MutationContext,
        account_id: ProviderAccountId,
    ) -> Result<AccountRefreshResult, AdminError>;

    async fn update(
        &self,
        context: &MutationContext,
        command: UpdateAccount,
    ) -> Result<AccountUpdateResult, AdminError>;

    async fn batch_update(
        &self,
        context: &MutationContext,
        command: BatchUpdateAccounts,
    ) -> Result<AccountsUpdateResult, AdminError>;

    async fn quota(
        &self,
        account_id: &ProviderAccountId,
        refresh: bool,
    ) -> Result<AccountDirectoryItem, AdminError>;

    async fn models(
        &self,
        account_id: &ProviderAccountId,
        refresh: bool,
    ) -> Result<ProviderModels, AdminError>;

    async fn test_connection(
        &self,
        account_id: ProviderAccountId,
        upstream_model: UpstreamModelId,
    ) -> Result<AccountConnectionTestEventStream, AdminError>;
}

pub(crate) struct DefaultAccountsService {
    accounts: Arc<dyn AccountStore>,
    providers: ProviderAdminRegistry,
    snapshot: Arc<dyn SnapshotControl>,
    probe: Arc<dyn AccountProbe>,
}

impl DefaultAccountsService {
    #[must_use]
    pub(crate) const fn new(
        accounts: Arc<dyn AccountStore>,
        providers: ProviderAdminRegistry,
        snapshot: Arc<dyn SnapshotControl>,
        probe: Arc<dyn AccountProbe>,
    ) -> Self {
        Self {
            accounts,
            providers,
            snapshot,
            probe,
        }
    }

    async fn load_account(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<AccountPageItem, AdminError> {
        self.accounts
            .load_account(account_id.as_str())
            .await
            .map_err(|error| map_store_error(error, "provider account"))?
            .ok_or_else(|| AdminError::not_found("Provider account was not found"))
    }

    async fn provider_for_account(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<
        (
            AccountPageItem,
            Arc<dyn crate::ports::provider::ProviderAdmin>,
        ),
        AdminError,
    > {
        let item = self.load_account(account_id).await?;
        let provider = self
            .providers
            .require(&item.account.provider_kind)
            .map_err(|error| map_provider_error(error, "provider account"))?;
        Ok((item, provider))
    }

    async fn attach_quota_local_usage(
        &self,
        accounts: &[AccountPageItem],
        quotas: &mut [ProviderQuota],
    ) -> Result<(), AdminError> {
        let windows = accounts
            .iter()
            .zip(quotas.iter())
            .flat_map(|(item, quota)| {
                quota
                    .windows
                    .iter()
                    .filter_map(|window| quota_usage_window(&item.account.id, window))
            })
            .collect::<Vec<_>>();
        if windows.is_empty() {
            return Ok(());
        }
        let usage_by_window = self
            .accounts
            .load_account_usage_by_windows(&windows)
            .await
            .map_err(|error| map_store_error(error, "quota window usage"))?
            .into_iter()
            .map(|result| ((result.account_id, result.key), result.usage))
            .collect::<BTreeMap<_, _>>();
        for (item, quota) in accounts.iter().zip(quotas) {
            for window in &mut quota.windows {
                if window.local_usage.is_none()
                    && window.local_usage_attribution == QuotaLocalUsageAttribution::AccountWide
                {
                    let key = (item.account.id.clone(), window.key.clone());
                    if let Some(usage) = usage_by_window.get(&key) {
                        window.local_usage = Some(usage.clone());
                    }
                }
            }
        }
        Ok(())
    }

    async fn load_directory_item(
        &self,
        account_id: &ProviderAccountId,
        refresh_quota: bool,
    ) -> Result<AccountDirectoryItem, AdminError> {
        let (stored, provider) = self.provider_for_account(account_id).await?;
        let account = &stored.account;
        let now = Utc::now();
        let rolling_range = TimeRange {
            start: now - Duration::hours(24),
            end: now,
        };
        let ids = vec![account.id.clone()];
        let rolling_usage = self
            .accounts
            .load_account_usage(rolling_range, &ids)
            .await
            .map_err(|error| map_store_error(error, "rolling account usage"))?;
        let rolling_usage = rolling_usage.into_iter().next();
        let mut quota = provider
            .quota(ProviderQuotaRequest {
                account_id: account_id.clone(),
                refresh: refresh_quota,
                rolling_usage: rolling_usage.clone(),
            })
            .await
            .map_err(|error| map_provider_error(error, "provider quota"))?;
        self.attach_quota_local_usage(
            std::slice::from_ref(&stored),
            std::slice::from_mut(&mut quota),
        )
        .await?;
        let usage = quota.representative_window_usage().cloned();
        Ok(AccountDirectoryItem {
            projection: stored.projection,
            usage,
            account: stored.account,
            quota,
        })
    }
}

#[async_trait]
impl AccountsService for DefaultAccountsService {
    async fn list(&self, query: AccountListQuery) -> Result<AccountDirectoryPage, AdminError> {
        let page = self
            .accounts
            .list_accounts(query)
            .await
            .map_err(|error| map_store_error(error, "account directory"))?;
        let now = Utc::now();
        let rolling_range = TimeRange {
            start: now - Duration::hours(24),
            end: now,
        };
        let ids = page
            .items
            .iter()
            .map(|item| item.account.id.clone())
            .collect::<Vec<_>>();
        let rolling_usage = self
            .accounts
            .load_account_usage(rolling_range, &ids)
            .await
            .map_err(|error| map_store_error(error, "rolling account usage"))?;
        let rolling_usage = rolling_usage
            .into_iter()
            .map(|usage| (usage.account_id.clone(), usage))
            .collect::<BTreeMap<_, _>>();
        let mut quotas = futures::future::join_all(page.items.iter().map(|item| async {
            let account = &item.account;
            let account_id = ProviderAccountId::new(account.id.clone())
                .map_err(|_| AdminError::invalid("Invalid provider account ID"))?;
            // 单个账号的 quota 投影失败（Provider 未注册或 quota 读取失败）不拖垮整页：
            // 该账号降级为空额度投影，其余账号与页面状态照常返回。
            let provider = match self.providers.require(&account.provider_kind) {
                Ok(provider) => provider,
                Err(error) => {
                    tracing::warn!(
                        account_id = %account.id,
                        error = %error,
                        "account directory provider is not registered; showing empty quota"
                    );
                    return Ok(empty_quota());
                }
            };
            match provider
                .quota(ProviderQuotaRequest {
                    account_id,
                    refresh: false,
                    rolling_usage: rolling_usage.get(&account.id).cloned(),
                })
                .await
            {
                Ok(quota) => Ok(quota),
                Err(error) => {
                    tracing::warn!(
                        account_id = %account.id,
                        error = %error,
                        "account directory quota projection failed; showing empty quota"
                    );
                    Ok(empty_quota())
                }
            }
        }))
        .await
        .into_iter()
        .collect::<Result<Vec<_>, AdminError>>()?;
        self.attach_quota_local_usage(&page.items, &mut quotas)
            .await?;
        let items = page
            .items
            .into_iter()
            .zip(quotas)
            .map(|(item, quota)| {
                let usage = quota.representative_window_usage().cloned();
                AccountDirectoryItem {
                    usage,
                    account: item.account,
                    projection: item.projection,
                    quota,
                }
            })
            .collect();
        Ok(AccountDirectoryPage {
            config_revision: page.config_revision,
            items,
            total: page.total,
            summary: page.summary,
        })
    }

    async fn export(
        &self,
        context: &MutationContext,
        account_ids: Vec<ProviderAccountId>,
    ) -> Result<AccountExportBundle, AdminError> {
        if account_ids.is_empty() || account_ids.len() > 200 {
            return Err(AdminError::invalid(
                "Account export requires between 1 and 200 accounts",
            ));
        }
        let exported_ids = account_ids.clone();
        let mut grouped = BTreeMap::<ProviderKind, Vec<ProviderAccountId>>::new();
        for account_id in account_ids {
            let account = self.load_account(&account_id).await?;
            grouped
                .entry(account.account.provider_kind)
                .or_default()
                .push(account_id);
        }
        if grouped.values().any(|ids| {
            let unique = ids.iter().collect::<std::collections::BTreeSet<_>>();
            unique.len() != ids.len()
        }) {
            return Err(AdminError::invalid("Account export contains duplicate IDs"));
        }
        let mut documents = Vec::with_capacity(grouped.len());
        for (provider_kind, ids) in grouped {
            let provider = self
                .providers
                .require(&provider_kind)
                .map_err(|error| map_provider_error(error, "provider account export"))?;
            let credentials = self
                .accounts
                .load_credentials_for_export(&provider_kind, &ids)
                .await
                .map_err(|error| map_store_error(error, "provider account export"))?;
            documents.push(
                provider
                    .export_credentials(credentials)
                    .await
                    .map_err(|error| map_provider_error(error, "provider account export"))?,
            );
        }
        self.accounts
            .record_credential_export(&exported_ids, context)
            .await
            .map_err(|error| map_store_error(error, "provider account export audit"))?;
        Ok(AccountExportBundle {
            exported_at: Utc::now(),
            documents,
        })
    }

    async fn refresh(
        &self,
        context: &MutationContext,
        account_id: ProviderAccountId,
    ) -> Result<AccountRefreshResult, AdminError> {
        let (stored, provider) = self.provider_for_account(&account_id).await?;
        let account = stored.account;
        let prepared = provider
            .prepare_refresh(PrepareCredentialRefresh {
                account: account.clone(),
            })
            .await
            .map_err(|error| map_provider_error(error, "provider credential refresh"))?;
        validate_prepared_rotation(&account, &prepared, "provider credential refresh")?;
        let result = commit_credential_refresh(
            self.accounts.as_ref(),
            prepared,
            context,
            "provider credential refresh",
        )
        .await?;
        provider
            .account_facts_changed(std::slice::from_ref(&result.account_id))
            .await;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        let account = self.load_directory_item(&result.account_id, false).await?;
        Ok(AccountRefreshResult {
            config_revision: result.config_revision,
            account,
        })
    }

    async fn recover(
        &self,
        context: &MutationContext,
        account_id: ProviderAccountId,
    ) -> Result<AccountRefreshResult, AdminError> {
        let (_, provider) = self.provider_for_account(&account_id).await?;
        let result = self
            .accounts
            .recover_account(&account_id, context)
            .await
            .map_err(|error| map_store_error(error, "provider account recovery"))?;
        provider
            .account_facts_changed(std::slice::from_ref(&account_id))
            .await;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        let account = self.load_directory_item(&account_id, false).await?;
        Ok(AccountRefreshResult {
            config_revision: result.config_revision,
            account,
        })
    }

    async fn update(
        &self,
        context: &MutationContext,
        command: UpdateAccount,
    ) -> Result<AccountUpdateResult, AdminError> {
        let account_id = ProviderAccountId::new(command.account_id.clone())
            .map_err(|_| AdminError::invalid("Invalid provider account ID"))?;
        let (_, provider) = self.provider_for_account(&account_id).await?;
        let enabled = command.enabled;
        let result = self
            .accounts
            .update_account(command, context)
            .await
            .map_err(|error| map_store_error(error, "provider account"))?;
        if !enabled {
            provider.account_unavailable(&account_id).await;
        }
        provider
            .account_facts_changed(std::slice::from_ref(&account_id))
            .await;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }

    async fn batch_update(
        &self,
        context: &MutationContext,
        command: BatchUpdateAccounts,
    ) -> Result<AccountsUpdateResult, AdminError> {
        let account_ids = command
            .account_ids
            .iter()
            .map(|id| {
                ProviderAccountId::new(id.clone())
                    .map_err(|_| AdminError::invalid("Invalid provider account ID"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut providers = BTreeMap::<
            ProviderKind,
            (
                Arc<dyn crate::ports::provider::ProviderAdmin>,
                Vec<ProviderAccountId>,
            ),
        >::new();
        for account_id in &account_ids {
            let (item, provider) = self.provider_for_account(account_id).await?;
            providers
                .entry(item.account.provider_kind)
                .or_insert_with(|| (provider, Vec::new()))
                .1
                .push(account_id.clone());
        }
        let enabled = command.enabled;
        let result = self
            .accounts
            .batch_update_accounts(command, context)
            .await
            .map_err(|error| map_store_error(error, "provider accounts"))?;
        for (provider, provider_ids) in providers.values() {
            if !enabled {
                for account_id in provider_ids {
                    provider.account_unavailable(account_id).await;
                }
            }
            provider.account_facts_changed(provider_ids).await;
        }
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }

    async fn quota(
        &self,
        account_id: &ProviderAccountId,
        refresh: bool,
    ) -> Result<AccountDirectoryItem, AdminError> {
        self.load_directory_item(account_id, refresh).await
    }

    async fn models(
        &self,
        account_id: &ProviderAccountId,
        refresh: bool,
    ) -> Result<ProviderModels, AdminError> {
        let (_, provider) = self.provider_for_account(account_id).await?;
        provider
            .models(account_id, refresh)
            .await
            .map_err(|error| map_provider_error(error, "provider model catalog"))
    }

    async fn test_connection(
        &self,
        account_id: ProviderAccountId,
        upstream_model: UpstreamModelId,
    ) -> Result<AccountConnectionTestEventStream, AdminError> {
        let (stored, provider) = self.provider_for_account(&account_id).await?;
        let account = stored.account;
        let model = upstream_model.as_str().to_owned();
        let operation = provider
            .connection_test_operation(&upstream_model, CONNECTION_TEST_INPUT)
            .map_err(|error| map_provider_error(error, "provider connection test"))?;
        let initial = vec![
            AccountConnectionTestEvent::Started {
                model: model.clone(),
            },
            AccountConnectionTestEvent::Request {
                model,
                input_text: CONNECTION_TEST_INPUT.to_owned(),
                stream: true,
                store: false,
            },
        ];
        let probe = Arc::clone(&self.probe);
        let terminal = futures::stream::once(async move {
            let result = probe
                .probe(AccountProbeRequest {
                    account_id,
                    provider_kind: account.provider_kind,
                    upstream_model,
                    operation,
                })
                .await;
            match result {
                Ok(result) => result
                    .text
                    .into_iter()
                    .map(|text| AccountConnectionTestEvent::Content { text })
                    .chain(std::iter::once(AccountConnectionTestEvent::Completed))
                    .collect(),
                Err(error) => {
                    let upstream_status = error
                        .upstream_response()
                        .map(gateway_core::engine::probe::AccountProbeUpstreamResponse::status);
                    let upstream_content_type = error
                        .upstream_response()
                        .and_then(|response| response.content_type())
                        .and_then(|value| std::str::from_utf8(value).ok())
                        .map(ToOwned::to_owned);
                    let upstream_body = error
                        .upstream_response()
                        .map(|response| String::from_utf8_lossy(response.body()).into_owned());
                    let message = upstream_body
                        .as_deref()
                        .filter(|body| !body.is_empty())
                        .unwrap_or_else(|| error.client_message())
                        .to_owned();
                    vec![AccountConnectionTestEvent::Failed {
                        message,
                        provider_error_code: error.client_error_code().map(ToOwned::to_owned),
                        provider_error_type: error.client_error_type().map(ToOwned::to_owned),
                        upstream_status,
                        upstream_content_type,
                        upstream_body,
                    }]
                }
            }
        })
        .flat_map(futures::stream::iter);
        Ok(Box::pin(futures::stream::iter(initial).chain(terminal)))
    }
}

/// 账号目录中单个账号 quota 读取失败时使用的空额度投影。
fn empty_quota() -> ProviderQuota {
    ProviderQuota {
        observed_at: None,
        refresh_token_expires_at: None,
        windows: Vec::new(),
        limit_reached: false,
        provider_data: None,
    }
}

fn quota_usage_window(
    account_id: &str,
    window: &ProviderQuotaWindow,
) -> Option<AccountUsageWindowQuery> {
    if window.local_usage.is_some()
        || window.local_usage_attribution != QuotaLocalUsageAttribution::AccountWide
    {
        return None;
    }
    let reset_at = window.reset_at?;
    let seconds = i64::try_from(window.window_seconds?).ok()?;
    let start = reset_at.checked_sub_signed(Duration::seconds(seconds))?;
    let range = TimeRange::new(start, reset_at).ok()?;
    // 上游百分比以该 reset 边界定义；以当前时间回推会让本地 Token 属于另一窗口。
    Some(AccountUsageWindowQuery {
        account_id: account_id.to_owned(),
        key: window.key.clone(),
        range,
    })
}
