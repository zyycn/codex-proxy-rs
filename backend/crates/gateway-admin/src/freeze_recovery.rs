//! 账号错误保护的到期探测与按冻结快照恢复

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use futures::StreamExt as _;
use gateway_core::account::{AccountErrorProtection, ProviderAccountId};
use gateway_core::routing::UpstreamModelId;
use gateway_core::task::{ScheduledTask, WorkerCycleContext, WorkerTaskError};
use tracing::warn;

use crate::model::accounts::{
    AccountConnectionTestEvent, AccountFreeze, AccountPageItem, AccountRuntimeSnapshot,
};
use crate::ports::store::{AccountRuntimeStore, AccountStore, SettingsStore};
use crate::use_case::accounts::AccountsService;

/// worker 周期；冷却观测与探测都依赖该节奏
pub const FREEZE_RECOVERY_INTERVAL: Duration = Duration::from_secs(30);
pub const FREEZE_RECOVERY_WORKER_OWNER: &str = "account-freeze-recovery";
pub const WORKER_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
pub const WORKER_MAXIMUM_BACKOFF: Duration = Duration::from_secs(60);
pub const WORKER_LEASE_TTL: Duration = Duration::from_secs(15 * 60);
pub const WORKER_LEASE_RENEWAL: Duration = Duration::from_secs(5 * 60);
/// 冻结恢复所需的管理端口组合
#[derive(Clone)]
pub struct FreezeRecoveryDeps {
    pub accounts: Arc<dyn AccountsService>,
    pub store: Arc<dyn AccountStore>,
    pub runtime: Arc<dyn AccountRuntimeStore>,
    pub settings: Arc<dyn SettingsStore>,
}

pub struct FreezeRecoveryTask {
    deps: FreezeRecoveryDeps,
}

impl FreezeRecoveryTask {
    pub fn new(deps: FreezeRecoveryDeps) -> Self {
        Self { deps }
    }

    /// 无法读取或校验配置时跳过本轮，不能把未知状态当作允许解冻
    async fn freeze_policy(&self) -> Option<AccountErrorProtection> {
        let settings = self.deps.settings.load_runtime_settings().await.ok()?;
        let policy = settings.values.account_error_protection;
        policy.validate().ok()?;
        Some(policy)
    }

    async fn active_freezes(&self) -> BTreeMap<String, AccountFreeze> {
        self.deps.runtime.active_freezes().await.unwrap_or_default()
    }

    async fn run_cycle_inner(&self) {
        let Some(policy) = self.freeze_policy().await else {
            return;
        };
        let freezes = self.active_freezes().await;
        if freezes.is_empty() {
            return;
        }
        self.recover_due_freezes(&freezes, &policy).await;
    }

    async fn load_account(&self, account_id: &str) -> Option<Option<AccountPageItem>> {
        let account_id = ProviderAccountId::new(account_id.to_owned()).ok()?;
        self.deps
            .store
            .load_account(account_id.as_str(), AccountRuntimeSnapshot::default())
            .await
            .ok()
    }

    /// 到达探测时间后才恢复；关闭自动冻结或探测时，已有冻结仍等待原冷却结束
    async fn recover_due_freezes(
        &self,
        freezes: &BTreeMap<String, AccountFreeze>,
        policy: &AccountErrorProtection,
    ) {
        for (account_id, freeze) in freezes {
            if freeze.until > Utc::now() {
                continue;
            }
            if policy.enabled && policy.probe_enabled && freeze.requires_probe {
                self.probe_and_recover(account_id, freeze, policy).await;
            } else if let Err(error) = self
                .deps
                .runtime
                .finish_freeze(account_id, freeze, None)
                .await
            {
                warn!(account_id, error = %error, "解除到期冻结失败");
            }
        }
    }

    async fn probe_and_recover(
        &self,
        account_id: &str,
        freeze: &AccountFreeze,
        policy: &AccountErrorProtection,
    ) {
        let Some(Some(item)) = self.load_account(account_id).await else {
            return;
        };
        if item.account.credential_revision != freeze.credential_revision {
            let _ = self
                .deps
                .runtime
                .finish_freeze(account_id, freeze, None)
                .await;
            return;
        }
        if !item.account.enabled {
            // 停用账号没有自动恢复意义；解冻交给管理员手动恢复
            return;
        }
        let Ok(account) = ProviderAccountId::new(account_id.to_owned()) else {
            return;
        };
        let Some(model) = self.resolve_probe_model(&account, freeze, policy).await else {
            // 没有可用探测模型时按失败处理，顺延冻结等待下一轮
            self.postpone(&account, freeze, policy).await;
            return;
        };
        let probe_succeeded = match self
            .deps
            .accounts
            .test_connection(account.clone(), model)
            .await
        {
            Ok(events) => drain_probe(events).await,
            Err(error) => {
                tracing::debug!(account_id, error = %error, "冻结恢复探测发起失败");
                false
            }
        };
        if probe_succeeded {
            match self
                .deps
                .runtime
                .finish_freeze(account_id, freeze, None)
                .await
            {
                Ok(true) => tracing::info!(account_id, "冻结恢复探测成功：账号已解冻"),
                Ok(false) => {}
                Err(error) => warn!(account_id, error = %error, "解除冻结失败"),
            }
        } else {
            self.postpone(&account, freeze, policy).await;
        }
    }

    /// 探测失败只顺延本次仍存在的冻结，旧探测不能重新冻结已手动恢复的账号
    async fn postpone(
        &self,
        account: &ProviderAccountId,
        freeze: &AccountFreeze,
        policy: &AccountErrorProtection,
    ) {
        // 探测期间关闭保护或探测时，不再把已经到期的冻结向后延长
        let Some(current) = self.freeze_policy().await else {
            return;
        };
        if !current.enabled || !current.probe_enabled {
            let _ = self
                .deps
                .runtime
                .finish_freeze(account.as_str(), freeze, None)
                .await;
            return;
        }
        let Some(until) = SystemTime::now().checked_add(Duration::from_secs(
            freeze.duration_seconds.unwrap_or(policy.duration_seconds),
        )) else {
            return;
        };
        let until = DateTime::<Utc>::from(until);
        match self
            .deps
            .runtime
            .finish_freeze(account.as_str(), freeze, Some(until))
            .await
        {
            Ok(true) => {
                tracing::info!(
                    account_id = account.as_str(),
                    postpone_seconds = Duration::from_secs(
                        freeze.duration_seconds.unwrap_or(policy.duration_seconds)
                    )
                    .as_secs(),
                    "冻结恢复探测失败：账号冻结顺延",
                );
            }
            Ok(false) => {}
            Err(error) => {
                warn!(
                    account_id = account.as_str(),
                    error = %error,
                    "冻结顺延失败"
                );
            }
        }
    }

    async fn resolve_probe_model(
        &self,
        account: &ProviderAccountId,
        freeze: &AccountFreeze,
        policy: &AccountErrorProtection,
    ) -> Option<UpstreamModelId> {
        if let Some(model) = if freeze.duration_seconds.is_some() {
            freeze.probe_model.as_deref()
        } else {
            policy.probe_model.as_deref()
        } {
            return UpstreamModelId::new(model.to_owned()).ok();
        }
        let models = self.deps.accounts.models(account, false).await.ok()?;
        models.models.into_iter().next().map(|model| model.id)
    }
}

impl ScheduledTask for FreezeRecoveryTask {
    fn run_cycle(
        &self,
        context: WorkerCycleContext,
    ) -> futures::future::BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            if context.cancellation().is_cancelled() {
                return Ok(());
            }
            self.run_cycle_inner().await;
            Ok(())
        })
    }
}

/// 消耗连接测试事件流并返回是否以 `Completed` 终止
async fn drain_probe(mut events: crate::model::accounts::AccountConnectionTestEventStream) -> bool {
    let mut completed = false;
    while let Some(event) = events.next().await {
        match event {
            AccountConnectionTestEvent::Completed => completed = true,
            AccountConnectionTestEvent::Failed { .. } => return false,
            _ => {}
        }
    }
    completed
}
