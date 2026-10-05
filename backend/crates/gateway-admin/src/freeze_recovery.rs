//! 容量熔断冻结的恢复编排：到期探测、失败顺延与自适应并发下调
//!
//! 冻结事实保存在可丢失的 Redis 冷却中，本服务只组合 Admin 端口：
//! 观测（`AccountRuntimeStore`）、探测（`AccountsService`）与写回
//! （原子降低并发 / 按冻结代次结束探测）
//! 旧观测不会覆盖管理员操作

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use futures::StreamExt as _;
use gateway_core::account::{AccountConcurrencyLimit, ProviderAccountId};
use gateway_core::routing::UpstreamModelId;
use gateway_core::task::{ScheduledTask, WorkerCycleContext, WorkerTaskError};
use tracing::warn;

use crate::model::accounts::{
    AccountConnectionTestEvent, AccountFreeze, AccountPageItem, AccountRuntimeSnapshot,
};
use crate::model::{MutationActor, MutationContext};
use crate::ports::store::{AccountRuntimeStore, AccountStore, SettingsStore};
use crate::use_case::accounts::AccountsService;

/// 自适应并发下调保留系数：下调到观测在途峰值的 80%
const ADAPTIVE_CONCURRENCY_FACTOR: f64 = 0.8;
/// 自适应并发下限：过低的并发让账号几乎不可用，宁可保持冻结
const ADAPTIVE_CONCURRENCY_FLOOR: u32 = 2;
/// worker 周期；冷却观测与探测都依赖该节奏
pub const FREEZE_RECOVERY_INTERVAL: Duration = Duration::from_secs(30);
pub const FREEZE_RECOVERY_WORKER_OWNER: &str = "account-freeze-recovery";
pub const WORKER_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
pub const WORKER_MAXIMUM_BACKOFF: Duration = Duration::from_secs(60);
pub const WORKER_LEASE_TTL: Duration = Duration::from_secs(15 * 60);
pub const WORKER_LEASE_RENEWAL: Duration = Duration::from_secs(5 * 60);
/// 系统审计上下文的固定请求 ID；账号并发更新审计由此溯源到自动冻结编排
const SYSTEM_REQUEST_ID: &str = "account-freeze-recovery";

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

    fn system_context() -> MutationContext {
        MutationContext {
            actor: MutationActor::System,
            request_id: SYSTEM_REQUEST_ID.to_owned(),
        }
    }

    /// 读取冻结策略；读取或校验失败跳过本轮，不能把未知配置当作允许解冻
    async fn freeze_policy(&self) -> Option<gateway_core::provider_ports::ProviderFreezePolicy> {
        let settings = self.deps.settings.load_runtime_settings().await.ok()?;
        gateway_core::provider_ports::ProviderFreezePolicy::try_new(
            settings.account_auto_freeze_enabled,
            settings.account_auto_freeze_threshold,
            settings.account_auto_freeze_window_seconds,
            settings.account_auto_freeze_duration_seconds,
            settings.account_auto_freeze_probe_enabled,
            settings.account_auto_freeze_probe_model,
            settings.account_auto_freeze_adaptive_concurrency,
        )
        .ok()
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
        if policy.enabled() && policy.adaptive_concurrency() {
            self.adapt_concurrency_limits(&freezes, &policy).await;
        }
        self.recover_due_freezes(&freezes, &policy).await;
    }

    /// 对冻结中的账号执行自适应并发下调：目标为观测在途峰值的 80%（下限 2），
    /// 只降不升；未观测到峰值证据的账号保持现状
    async fn adapt_concurrency_limits(
        &self,
        freezes: &BTreeMap<String, AccountFreeze>,
        policy: &gateway_core::provider_ports::ProviderFreezePolicy,
    ) {
        let account_ids = freezes.keys().cloned().collect::<Vec<_>>();
        let Ok(peaks) = self.deps.runtime.capacity_peaks(&account_ids).await else {
            return;
        };
        for (account_id, peak) in peaks {
            let Some(target) = adaptive_target(peak) else {
                continue;
            };
            let Ok(account) = ProviderAccountId::new(account_id.clone()) else {
                continue;
            };
            let Some(limit) = AccountConcurrencyLimit::new(target) else {
                continue;
            };
            match self
                .deps
                .accounts
                .lower_concurrency_limit(&Self::system_context(), account, limit)
                .await
            {
                Ok(Some(_)) => {
                    tracing::info!(
                        account_id,
                        adapted_limit = target,
                        freeze_seconds = policy.freeze_duration().as_secs(),
                        "容量熔断：按观测在途峰值下调账号并发上限",
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    warn!(account_id, error = %error, "自适应并发下调失败");
                }
            }
        }
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
        policy: &gateway_core::provider_ports::ProviderFreezePolicy,
    ) {
        for (account_id, freeze) in freezes {
            if freeze.until > Utc::now() {
                continue;
            }
            if policy.enabled() && policy.probe_enabled() && freeze.requires_probe {
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
        policy: &gateway_core::provider_ports::ProviderFreezePolicy,
    ) {
        let Some(Some(item)) = self.load_account(account_id).await else {
            return;
        };
        if !item.account.enabled {
            // 停用账号没有自动恢复意义；解冻交给管理员手动恢复
            return;
        }
        let Ok(account) = ProviderAccountId::new(account_id.to_owned()) else {
            return;
        };
        let Some(model) = self.resolve_probe_model(&account, policy).await else {
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
        policy: &gateway_core::provider_ports::ProviderFreezePolicy,
    ) {
        let Some(until) = SystemTime::now().checked_add(policy.freeze_duration()) else {
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
                    postpone_seconds = policy.freeze_duration().as_secs(),
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
        policy: &gateway_core::provider_ports::ProviderFreezePolicy,
    ) -> Option<UpstreamModelId> {
        if let Some(model) = policy.probe_model() {
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

/// 自适应目标并发：观测峰值 × 0.8 向下取整，下限 2；无证据（峰值为 0）不调整
fn adaptive_target(peak_in_flight: u32) -> Option<u32> {
    if peak_in_flight == 0 {
        return None;
    }
    let scaled = (f64::from(peak_in_flight) * ADAPTIVE_CONCURRENCY_FACTOR).floor() as u32;
    Some(scaled.max(ADAPTIVE_CONCURRENCY_FLOOR))
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
